use std::io::Read;
use std::net::IpAddr;
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use sealed_diary_core::{ObjectId, ObjectKind};
use ureq::http::{Request, Response};
use ureq::{Agent, Body, SendBody};

use crate::remote::{Remote, RemotePath};
use crate::{Result, SyncError};

const MAX_LISTING_LEN: u64 = 256 << 20;

/// A WebDAV folder: Nextcloud, a NAS, or a small server the user runs.
pub struct WebDav {
    agent: Agent,
    /// Always ends in `/`.
    base: String,
    authorization: String,
}

impl WebDav {
    pub fn new(url: &str, username: &str, password: &str) -> Result<Self> {
        check_url(url)?;
        let agent = Agent::config_builder()
            .http_status_as_error(false)
            .allow_non_standard_methods(true)
            .max_redirects(0)
            .timeout_connect(Some(Duration::from_secs(15)))
            .timeout_recv_response(Some(Duration::from_secs(60)))
            .build()
            .into();
        let credentials = STANDARD.encode(format!("{username}:{password}"));
        Ok(Self {
            agent,
            base: format!("{}/", url.trim_end_matches('/')),
            authorization: format!("Basic {credentials}"),
        })
    }

    /// Checks the address and credentials and creates the folders the vault
    /// needs. Call once when the user sets sync up.
    pub fn prepare(&self) -> Result<()> {
        for kind in [ObjectKind::Entry, ObjectKind::Media] {
            let response = self.run("MKCOL", kind.dir(), &[], ())?;
            // 405 is how a server says the folder is already there.
            if !response.status().is_success() && response.status().as_u16() != 405 {
                return Err(SyncError::Status(response.status().as_u16()));
            }
        }
        Ok(())
    }

    fn run(
        &self,
        method: &str,
        path: &str,
        headers: &[(&str, String)],
        body: impl ureq::AsSendBody,
    ) -> Result<Response<Body>> {
        let mut request = Request::builder()
            .method(method)
            .uri(format!("{}{path}", self.base))
            .header("Authorization", &self.authorization);
        for (name, value) in headers {
            request = request.header(*name, value);
        }
        let request = request
            .body(body)
            .map_err(|e| SyncError::Network(e.to_string()))?;
        let response = self
            .agent
            .run(request)
            .map_err(|e| SyncError::Network(e.to_string()))?;
        match response.status().as_u16() {
            401 | 403 => Err(SyncError::Unauthorized),
            _ => Ok(response),
        }
    }
}

impl Remote for WebDav {
    fn list(&self, kind: ObjectKind) -> Result<Vec<ObjectId>> {
        let depth = [("Depth", "1".to_owned())];
        let response = self.run("PROPFIND", &format!("{}/", kind.dir()), &depth, ())?;
        match response.status().as_u16() {
            404 => return Ok(Vec::new()),
            status if !response.status().is_success() => return Err(SyncError::Status(status)),
            _ => {}
        }
        let mut listing = String::new();
        response
            .into_body()
            .into_reader()
            .take(MAX_LISTING_LEN)
            .read_to_string(&mut listing)?;
        Ok(ids_in_listing(&listing))
    }

    fn download(&self, path: &RemotePath) -> Result<Option<Box<dyn Read>>> {
        let response = self.run("GET", &path.relative(), &[], ())?;
        match response.status().as_u16() {
            404 => Ok(None),
            status if !response.status().is_success() => Err(SyncError::Status(status)),
            _ => Ok(Some(Box::new(response.into_body().into_reader()))),
        }
    }

    fn upload(&self, path: &RemotePath, body: &mut dyn Read, len: u64) -> Result<()> {
        // An explicit length, because several WebDAV servers refuse the
        // chunked uploads a body of unknown size would be sent as.
        let headers = [("Content-Length", len.to_string())];
        let body = SendBody::from_reader(body);
        let status = self.run("PUT", &path.relative(), &headers, body)?.status();
        if status.is_success() {
            Ok(())
        } else {
            Err(SyncError::Status(status.as_u16()))
        }
    }

    fn delete(&self, path: &RemotePath) -> Result<()> {
        let status = self.run("DELETE", &path.relative(), &[], ())?.status();
        if status.is_success() || status.as_u16() == 404 {
            Ok(())
        } else {
            Err(SyncError::Status(status.as_u16()))
        }
    }
}

/// Object ids named in a PROPFIND response.
///
/// Rather than parse the XML, this takes the text after every `href>` and
/// keeps the last path segment when it is an object id. That reads every
/// namespace prefix servers use, and a listing is untrusted anyway: whatever
/// it names still has to authenticate when fetched.
fn ids_in_listing(listing: &str) -> Vec<ObjectId> {
    listing
        .split("href>")
        .skip(1)
        .filter_map(|rest| {
            let href = rest.split('<').next()?.trim().trim_end_matches('/');
            ObjectId::from_hex(href.rsplit('/').next()?).ok()
        })
        .collect()
}

/// Plain `http` would send the server password in the clear, so it is only
/// accepted where the network itself is private: this machine, a home LAN, or
/// a VPN such as WireGuard or Tailscale.
fn check_url(url: &str) -> Result<()> {
    if url.starts_with("https://") {
        return Ok(());
    }
    let Some(rest) = url.strip_prefix("http://") else {
        return Err(SyncError::InvalidUrl("must start with https://"));
    };
    let authority = rest.split('/').next().unwrap_or_default();
    let host = match authority.strip_prefix('[') {
        Some(v6) => v6.split(']').next().unwrap_or_default(),
        None => authority.split(':').next().unwrap_or_default(),
    };
    let private = match host.parse::<IpAddr>() {
        Ok(IpAddr::V4(ip)) => {
            let [a, b, ..] = ip.octets();
            ip.is_loopback()
                || ip.is_private()
                || ip.is_link_local()
                || (a == 100 && b & 0xc0 == 64)
        }
        Ok(IpAddr::V6(ip)) => ip.is_loopback() || ip.segments()[0] & 0xfe00 == 0xfc00,
        Err(_) => host == "localhost",
    };
    if private {
        Ok(())
    } else {
        Err(SyncError::InvalidUrl(
            "http:// is only allowed for private network addresses; use https://",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn http_only_on_private_networks() {
        for ok in [
            "https://dav.example.org/diario",
            "http://localhost:8080",
            "http://127.0.0.1:8080/x",
            "http://192.168.1.20/dav",
            "http://10.0.0.5",
            "http://100.101.102.103:8080",
            "http://[fd7a:115c:a1e0::1]:8080/x",
            "http://[::1]/",
        ] {
            assert!(check_url(ok).is_ok(), "{ok}");
        }
        for bad in [
            "http://dav.example.org",
            "http://8.8.8.8/dav",
            "http://100.200.1.1",
            "http://localhost.evil.example",
            "http://user@192.168.1.2@evil.example",
            "ftp://192.168.1.2",
            "192.168.1.2",
        ] {
            assert!(check_url(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn listing_ids_from_different_servers() {
        let a = "0123456789abcdef0123456789abcdef";
        let b = "fedcba9876543210fedcba9876543210";
        let listing = format!(
            "<?xml version=\"1.0\"?><D:multistatus xmlns:D=\"DAV:\">\
             <D:response><D:href>/dav/entries/</D:href></D:response>\
             <D:response><D:href>/dav/entries/{a}</D:href></D:response>\
             <d:response><d:href>http://host/dav/entries/{b}</d:href></d:response>\
             <response><href>/dav/entries/notes.txt</href></response>\
             <response><href>/dav/entries/{a}.partial</href></response></D:multistatus>"
        );
        let ids: Vec<_> = ids_in_listing(&listing)
            .iter()
            .map(ObjectId::to_hex)
            .collect();
        assert_eq!(ids, [a, b]);
    }
}
