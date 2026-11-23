//! The web pages' Content-Security-Policy: each page may run only its own script, named by its hash, so a script
//! that got into a page some other way (a song title with markup, say) doesn't run. Its inline styles stay allowed.

use base64::Engine;
use sha2::{Digest, Sha256};

/// `policy` with its `script-src 'unsafe-inline'` replaced by the hashes of the page's own inline scripts.
pub fn csp(page: &str, policy: &str) -> String {
    let mut hashes = vec![];
    let mut rest = page;
    while let Some(i) = rest.find("<script") {
        let after = &rest[i..];
        let Some(open_end) = after.find('>') else { break };
        let body = &after[open_end + 1..];
        let Some(close) = body.find("</script>") else { break };
        let digest = Sha256::digest(&body.as_bytes()[..close]);
        hashes.push(format!("'sha256-{}'", base64::engine::general_purpose::STANDARD.encode(digest)));
        rest = &body[close..];
    }
    policy.replace("script-src 'unsafe-inline'", &format!("script-src {}", hashes.join(" ")))
}

/// For the pages only the server's admins use (Podium, setup): nothing from elsewhere, never inside another site's
/// frame (so nobody can lay their page over it and have an admin click through).
pub const ADMIN_POLICY: &str = "default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; connect-src 'self'; \
                                img-src 'self' data:; base-uri 'none'; form-action 'self'; frame-ancestors 'none'";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_pages_own_scripts() {
        let p = "<html><script>let a = 1;</script><p>x</p><script type=\"module\">go()</script></html>";
        let c = csp(p, "default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'");
        let h = |s: &str| format!("'sha256-{}'", base64::engine::general_purpose::STANDARD.encode(Sha256::digest(s.as_bytes())));
        assert_eq!(c, format!("default-src 'none'; script-src {} {}; style-src 'unsafe-inline'", h("let a = 1;"), h("go()")));
        // every real page has exactly its one script hashed, and no 'unsafe-inline' scripts
        for page in [
            include_str!("../web/admin/index.html"),
            include_str!("../web/player/index.html"),
            include_str!("../web/setup/index.html"),
            include_str!("../web/jam/index.html"),
            include_str!("../web/collab/index.html"),
            include_str!("../web/share/index.html"),
        ] {
            let c = csp(page, ADMIN_POLICY);
            assert_eq!(c.matches("'sha256-").count(), 1, "{c}");
            assert!(!c.split(';').any(|d| d.trim().starts_with("script-src") && d.contains("unsafe-inline")), "{c}");
        }
    }
}
