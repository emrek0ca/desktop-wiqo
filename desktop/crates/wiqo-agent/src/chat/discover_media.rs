use std::{sync::OnceLock, time::Duration};

use anyhow::{anyhow, Result};
use base64::{engine::general_purpose::STANDARD, Engine};
use reqwest::{header, redirect::Policy, Client, StatusCode, Url};
use tokio::sync::Semaphore;

const MAX_IMAGE_BYTES: usize = 4_000_000;
const IMAGE_TIMEOUT: Duration = Duration::from_secs(15);

pub(super) struct DiscoverMedia {
    client: OnceLock<Option<Client>>,
    slots: Semaphore,
}

impl DiscoverMedia {
    pub fn new() -> Self {
        Self {
            client: OnceLock::new(),
            slots: Semaphore::new(6),
        }
    }

    pub async fn image(&self, server: &str, source: &str) -> Result<String> {
        tokio::time::timeout(IMAGE_TIMEOUT, self.read_image(server, source))
            .await
            .ok()
            .flatten()
            .ok_or_else(|| anyhow!("Keşfet görseli açılamadı."))
    }

    async fn read_image(&self, server: &str, source: &str) -> Option<String> {
        let (url, avatar) = public_image_url(server, source)?;
        let _slot = self.slots.acquire().await.ok()?;
        let client = self
            .client
            .get_or_init(|| Client::builder().redirect(Policy::none()).build().ok())
            .as_ref()?;
        let mut response = client.get(url).send().await.ok()?;
        if avatar
            && matches!(
                response.status(),
                StatusCode::MOVED_PERMANENTLY
                    | StatusCode::FOUND
                    | StatusCode::SEE_OTHER
                    | StatusCode::TEMPORARY_REDIRECT
                    | StatusCode::PERMANENT_REDIRECT
            )
        {
            let target = response.headers().get(header::LOCATION)?.to_str().ok()?;
            response = client.get(google_avatar_url(target)?).send().await.ok()?;
        }
        if response.status() != StatusCode::OK
            || response
                .content_length()
                .is_some_and(|size| size > MAX_IMAGE_BYTES as u64)
        {
            return None;
        }
        let mime = match response
            .headers()
            .get(header::CONTENT_TYPE)?
            .to_str()
            .ok()?
            .split(';')
            .next()?
            .trim()
        {
            "image/jpeg" => "image/jpeg",
            "image/png" => "image/png",
            "image/webp" => "image/webp",
            "image/gif" => "image/gif",
            _ => return None,
        };
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.ok()? {
            if bytes.len() + chunk.len() > MAX_IMAGE_BYTES {
                return None;
            }
            bytes.extend_from_slice(&chunk);
        }
        let raster = match mime {
            "image/jpeg" => bytes.starts_with(&[0xff, 0xd8, 0xff]),
            "image/png" => bytes.starts_with(b"\x89PNG\r\n\x1a\n"),
            "image/webp" => bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP"),
            "image/gif" => bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a"),
            _ => false,
        };
        raster.then(|| format!("data:{mime};base64,{}", STANDARD.encode(bytes)))
    }
}

fn public_image_url(server: &str, source: &str) -> Option<(Url, bool)> {
    let server = Url::parse(server).ok()?;
    let url = Url::parse(source).ok()?;
    if !matches!(url.scheme(), "http" | "https")
        || url.origin() != server.origin()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return None;
    }
    let (kind, key) = url.path().strip_prefix("/missions/")?.split_once('/')?;
    let avatar = match kind {
        "avatars"
            if (3..=20).contains(&key.len())
                && key
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'.')) =>
        {
            true
        }
        "images" | "covers"
            if key.len() == 24
                && key
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-')) =>
        {
            false
        }
        _ => return None,
    };
    Some((url, avatar))
}

fn google_avatar_url(target: &str) -> Option<Url> {
    let url = Url::parse(target).ok()?;
    (url.scheme() == "https"
        && url.host_str()?.ends_with(".googleusercontent.com")
        && url.port().is_none()
        && url.username().is_empty()
        && url.password().is_none()
        && url.fragment().is_none())
    .then_some(url)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};

    #[test]
    fn public_media_is_limited_to_the_configured_origin_and_known_asset_paths() {
        let server = "https://api.wiqo.invalid";
        assert!(
            public_image_url(
                server,
                "https://api.wiqo.invalid/missions/avatars/user.name"
            )
            .unwrap()
            .1
        );
        assert!(
            !public_image_url(
                server,
                "https://api.wiqo.invalid/missions/images/0123456789_ABCDEFGHIJKLM"
            )
            .unwrap()
            .1
        );
        assert!(public_image_url(
            server,
            "https://api.wiqo.invalid/missions/covers/0123456789_ABCDEFGHIJKLM"
        )
        .is_some());
        assert!(
            public_image_url(server, "https://other.invalid/missions/avatars/user.name").is_none()
        );
        assert!(public_image_url(server, "https://api.wiqo.invalid/profile").is_none());
        assert!(public_image_url(
            server,
            "https://api.wiqo.invalid/missions/avatars/user.name?api=x"
        )
        .is_none());
        assert!(public_image_url(
            server,
            "https://api.wiqo.invalid/missions/avatars/user.name%2fmore"
        )
        .is_none());
        assert!(google_avatar_url("https://lh3.googleusercontent.com/a/user").is_some());
        assert!(google_avatar_url("https://lh3.googleusercontent.com.invalid/a/user").is_none());
        assert!(
            google_avatar_url("https://other.invalid@lh3.googleusercontent.com/a/user").is_none()
        );
    }

    #[tokio::test]
    async fn native_media_loads_public_bytes_when_the_webview_is_blocked() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let host = format!("http://{}", listener.local_addr().unwrap());
        let png = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+aDdsAAAAASUVORK5CYII=";
        let bytes = STANDARD.decode(png).unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut headers = BufReader::new(&stream).lines();
            assert_eq!(
                headers.next().unwrap().unwrap(),
                "GET /missions/avatars/test.user HTTP/1.1"
            );
            for line in headers
                .map_while(Result::ok)
                .take_while(|line| !line.is_empty())
            {
                assert!(!line.to_ascii_lowercase().starts_with("authorization:"));
            }
            write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nCross-Origin-Resource-Policy: same-origin\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", bytes.len()).unwrap();
            stream.write_all(&bytes).unwrap();
        });
        let image = DiscoverMedia::new()
            .image(&host, &format!("{host}/missions/avatars/test.user"))
            .await
            .unwrap();
        assert_eq!(image, format!("data:image/png;base64,{png}"));
        server.join().unwrap();
    }
}
