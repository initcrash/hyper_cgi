//! This module implements a do_cgi function, to run CGI scripts with hyper
use futures::TryStreamExt;
use hyper::{Request, Response};
use std::process::Stdio;
use std::str::FromStr;
use tokio::io::AsyncBufReadExt;
use tokio::io::AsyncReadExt;
use tokio::io::AsyncWriteExt;
use tokio::io::BufReader;
use tokio::process::Command;
use tokio::stream::StreamExt;

/// do_cgi is an async function that takes an hyper request and a CGI compatible
/// command, and passes the request to be executed to the command.
/// It then returns an hyper response and the stderr output of the command.
pub async fn do_cgi(
    req: Request<hyper::Body>,
    cmd: Command,
) -> (hyper::http::Response<hyper::Body>, Vec<u8>) {
    let mut cmd = cmd;
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    cmd.stdin(Stdio::piped());
    cmd.env("SERVER_SOFTWARE", "hyper")
        .env("SERVER_NAME", "localhost") // TODO
        .env("GATEWAY_INTERFACE", "CGI/1.1")
        .env("SERVER_PROTOCOL", "HTTP/1.1") // TODO
        .env("SERVER_PORT", "80") // TODO
        .env("REQUEST_METHOD", format!("{}", req.method()))
        .env("SCRIPT_NAME", "") // TODO
        .env("QUERY_STRING", req.uri().query().unwrap_or(""))
        .env("REMOTE_ADDR", "") // TODO
        .env("AUTH_TYPE", "") // TODO
        .env("REMOTE_USER", "") // TODO
        .env(
            "CONTENT_TYPE",
            req.headers()
                .get(hyper::header::CONTENT_TYPE)
                .map(|x| x.to_str().ok())
                .flatten()
                .unwrap_or(""),
        )
        .env(
            "HTTP_CONTENT_ENCODING",
            req.headers()
                .get(hyper::header::CONTENT_ENCODING)
                .map(|x| x.to_str().ok())
                .flatten()
                .unwrap_or(""),
        )
        .env(
            "CONTENT_LENGTH",
            req.headers()
                .get(hyper::header::CONTENT_LENGTH)
                .map(|x| x.to_str().ok())
                .flatten()
                .unwrap_or(""),
        );

    let mut child = cmd.spawn().expect("can't spawn CGI command");
    let mut stdin = child.stdin.as_mut().expect("Failed to open stdin");
    let mut stdout = child.stdout.as_mut().expect("Failed to open stdout");
    let mut stderr = child.stderr.as_mut().expect("Failed to open stderr");

    let req_body = req
        .into_body()
        .map(|result| {
            result.map_err(|_error| std::io::Error::new(std::io::ErrorKind::Other, "Error!"))
        })
        .into_async_read();

    let mut req_body = to_tokio_async_read(req_body);
    let mut err_output = vec![];

    let res = tokio::try_join!(
        async {
            println!("copy");
            tokio::io::copy(&mut req_body, &mut stdin).await?;
            println!("copy done");
            stdin.shutdown().await?;
            println!("done");
            Ok(())
        },
        {
        //tokio::io::copy(&mut stdout, &mut tokio::io::stdout()).await;
            println!("build response");

        build_response(&mut stdout, &mut stderr, &mut err_output)
        }
    );

    let (_, r2) = res.unwrap_or((
        (),
        Response::builder()
            .status(hyper::StatusCode::INTERNAL_SERVER_ERROR)
            .body(hyper::Body::empty())
            .unwrap(),
    ));

    (r2, err_output)
}

fn to_tokio_async_read(r: impl futures::io::AsyncRead) -> impl tokio::io::AsyncRead {
    tokio_util::compat::FuturesAsyncReadCompatExt::compat(r)
}

async fn build_response(
    stdout: &mut &mut tokio::process::ChildStdout,
    stderr: &mut &mut tokio::process::ChildStderr,
    err_output: &mut Vec<u8>,
) -> Result<Response<hyper::Body>, std::io::Error> {
    let mut response = Response::builder();

    let mut stdout = BufReader::new(stdout);
    let mut line = String::new();
    while stdout.read_line(&mut line).await.unwrap_or(0) > 0 {
        line = line
            .trim_end_matches("\n")
            .trim_end_matches("\r")
            .to_owned();

        let l: Vec<&str> = line.splitn(2, ": ").collect();
        if l.len() < 2 {
            break;
        }
        if l[0] == "Status" {
            response = response.status(
                hyper::StatusCode::from_u16(
                    u16::from_str(l[1].split(" ").next().unwrap_or("500")).unwrap_or(500),
                )
                .unwrap_or(hyper::StatusCode::INTERNAL_SERVER_ERROR),
            );
        } else {
            response = response.header(l[0], l[1]);
        }
        line = String::new();
    }

    stderr.read_to_end(err_output).await.unwrap_or(0);

    let mut data = vec![];
    stdout.read_to_end(&mut data).await.unwrap_or(0);

    let body = response.body(hyper::Body::from(data));

    convert_error_io_hyper(body)
}

fn convert_error_io_hyper<T>(res: Result<T, hyper::http::Error>) -> Result<T, std::io::Error> {
    match res {
        Ok(res) => Ok(res),
        Err(_) => Err(std::io::Error::new(std::io::ErrorKind::Other, "Error!")),
    }
}

#[cfg(test)]
mod tests {
    use futures::TryStreamExt;

    #[tokio::test]
    async fn run_cmd() {
        let body_content = "a body";

        let req = hyper::Request::builder()
            .method("GET")
            .uri("/some/file?query=aquery")
            .version(hyper::Version::HTTP_11)
            .header("Host", "localhost:8001")
            .header("User-Agent", "test/2.25.1")
            .header("Accept", "*/*")
            .header("Accept-Encoding", "deflate, gzip, br")
            .header("Accept-Language", "en-US, *;q=0.9")
            .header("Pragma", "no-cache")
            .body(hyper::Body::from(body_content))
            .unwrap();

        let mut cmd = tokio::process::Command::new("echo");
        cmd.arg("-n");
        cmd.arg("blabl:bl\r\na body");

        let (rep, stderr) = super::do_cgi(req, cmd).await;
        let output = rep
            .into_body()
            .try_fold(String::new(), |mut acc, elt| async move {
                acc.push_str(std::str::from_utf8(&elt).unwrap());
                Ok(acc)
            }).await.unwrap();
        assert_eq!("", std::str::from_utf8(&stderr).unwrap());
        assert_eq!(body_content, output);
    }
}
