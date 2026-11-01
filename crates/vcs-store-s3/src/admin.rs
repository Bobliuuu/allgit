//! Deployment self-checks that are not part of the storage protocol (docs/deploy.md, "S3 with a sole
//! manifest writer"). A child module of the crate root so it can use the private signed-request path.

use super::{BackendError, Resp, S3Backend};

/// Result of [`S3Backend::probe_manifest_write`]: what the credential in this client can do to the
/// manifest key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManifestWriteProbe {
    /// Every write route was refused with 401/403 (single PUT and multipart initiation).
    Denied,
    /// At least one route was NOT refused: the credential can write the manifest. Holds what was tried.
    Allowed(String),
}

impl S3Backend {
    /// Try, with THIS client's credential, to write the manifest key without ever changing it:
    /// a PUT guarded by `If-Match` on an ETag that cannot exist (an authorised store answers 412 or
    /// 404 and writes nothing; the provider was verified to honour `If-Match` by [`S3Backend::connect`])
    /// and a CreateMultipartUpload on the same key (an upload that is started is aborted at once;
    /// the key is untouched until CompleteMultipartUpload). Run it with a *client* credential to show
    /// the bucket policy denies clients the manifest; run with the server's own it would say Allowed.
    pub fn probe_manifest_write(&self) -> Result<ManifestWriteProbe, BackendError> {
        let key = format!("{}{}", self.cfg.prefix, vcs_remote::manifest::KEY);
        let denied = |c: u16| matches!(c, 401 | 403);
        let mut allowed: Vec<String> = Vec::new();
        let stale = format!("\"{}\"", "0".repeat(32));
        match self.send("PUT", &key, &[], &[("if-match", stale.as_str())], b"selfcheck")? {
            Resp::Status(c, _) if denied(c) => {}
            Resp::Status(c @ (404 | 409 | 412), _) => allowed.push(format!("conditional PUT answered {c} (not refused)")),
            Resp::Status(c, m) => return Err(BackendError::Other(format!("manifest write probe inconclusive: PUT {c} {m}"))),
            Resp::Ok { .. } => allowed.push("conditional PUT with a bogus If-Match SUCCEEDED".into()),
        }
        match self.send("POST", &key, &[("uploads", String::new())], &[], b"")? {
            Resp::Status(c, _) if denied(c) => {}
            Resp::Status(c, m) => return Err(BackendError::Other(format!("manifest write probe inconclusive: multipart {c} {m}"))),
            Resp::Ok { body, .. } => {
                allowed.push("CreateMultipartUpload was accepted".into());
                let text = String::from_utf8_lossy(&body);
                if let Some(id) = text.split("<UploadId>").nth(1).and_then(|r| r.split("</UploadId>").next()) {
                    let _ = self.send("DELETE", &key, &[("uploadId", id.to_owned())], &[], b"");
                }
            }
        }
        Ok(if allowed.is_empty() { ManifestWriteProbe::Denied } else { ManifestWriteProbe::Allowed(allowed.join("; ")) })
    }

    /// Install a bucket policy (PutBucketPolicy). For deployment scripts and tests; needs a credential
    /// allowed to manage the bucket, never a client credential.
    pub fn put_bucket_policy(&self, policy_json: &str) -> Result<(), BackendError> {
        match self.send("PUT", "", &[("policy", String::new())], &[("content-type", "application/json")], policy_json.as_bytes())? {
            Resp::Ok { .. } => Ok(()),
            Resp::Status(c, m) => Err(BackendError::Other(format!("put bucket policy: {c} {m}"))),
        }
    }

    /// Remove the bucket policy (DeleteBucketPolicy); absent is fine.
    pub fn delete_bucket_policy(&self) -> Result<(), BackendError> {
        match self.send("DELETE", "", &[("policy", String::new())], &[], b"")? {
            Resp::Ok { .. } | Resp::Status(404, _) => Ok(()),
            Resp::Status(c, m) => Err(BackendError::Other(format!("delete bucket policy: {c} {m}"))),
        }
    }
}
