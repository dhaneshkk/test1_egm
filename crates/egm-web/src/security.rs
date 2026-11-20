//! Response headers on every answer, error pages included (SOC 2 CC6.6, CC6.7): a strict
//! content security policy (scripts only from this origin; the app has one script, the job log
//! follower), no framing, no MIME sniffing, no referrer to other sites, and HSTS when the app is
//! served over HTTPS. Cross-site form posts are refused by Topcoat's origin policy.

use topcoat::context::Cx;
use topcoat::router::response::response_headers;
use topcoat::router::{header, Body, HeaderValue, Layer, LayerFuture, Next, Path};

pub const CSP: &str = "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; \
                       connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'; object-src 'none'";

pub struct SecurityHeaders {
    pub hsts: bool,
}

impl Layer for SecurityHeaders {
    fn path(&self) -> Option<&Path> {
        None
    }

    fn handle<'a>(&'a self, cx: &'a Cx, body: Body, next: Next<'a>) -> LayerFuture<'a> {
        Box::pin(async move {
            let h = response_headers(cx);
            h.append(header::CONTENT_SECURITY_POLICY, HeaderValue::from_static(CSP));
            h.append(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
            h.append(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
            h.append(header::REFERRER_POLICY, HeaderValue::from_static("same-origin"));
            if self.hsts {
                h.append(header::STRICT_TRANSPORT_SECURITY, HeaderValue::from_static("max-age=31536000; includeSubDomains"));
            }
            next.run(cx, body).await
        })
    }
}
