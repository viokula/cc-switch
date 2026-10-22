use std::fs;
use std::path::{Path, PathBuf};

use crate::config::atomic_write;
use crate::config::get_app_config_dir;
use crate::error::AppError;

const CERT_DIR_NAME: &str = "tls-certs";
const CERT_FILE: &str = "proxy-cert.pem";
const KEY_FILE: &str = "proxy-key.pem";
const LEAF_CERT_FILE: &str = "proxy-leaf.pem";

const CA_CN: &str = "cc-switch Huawei Proxy CA";
const CERT_CN: &str = "platform.claude.com";
const SAN_DOMAINS: &[&str] = &[
    "api.anthropic.com",
    "platform.claude.com",
    "*.claude.com",
    "*.anthropic.com",
];

pub struct TlsCertManager {
    cert_dir: PathBuf,
}

impl TlsCertManager {
    pub fn new() -> Self {
        Self {
            cert_dir: get_app_config_dir().join(CERT_DIR_NAME),
        }
    }

    pub fn cert_path(&self) -> PathBuf {
        self.cert_dir.join(CERT_FILE)
    }

    pub fn key_path(&self) -> PathBuf {
        self.cert_dir.join(KEY_FILE)
    }

    /// 叶证书路径。CA 证书在 [`Self::cert_path`]，由客户端作为信任根安装；
    /// 服务端把它签发的叶证书（链：叶 + CA）出示给客户端。
    pub fn leaf_cert_path(&self) -> PathBuf {
        self.cert_dir.join(LEAF_CERT_FILE)
    }

    pub fn ensure_cert(&self) -> Result<(PathBuf, PathBuf), AppError> {
        let cert_path = self.cert_path();
        let key_path = self.key_path();

        if cert_path.exists() && key_path.exists() && self.leaf_cert_path().exists() {
            return Ok((cert_path, key_path));
        }

        self.generate_cert(&cert_path, &key_path)?;
        Ok((cert_path, key_path))
    }

    fn generate_cert(&self, cert_path: &Path, key_path: &Path) -> Result<(), AppError> {
        use rcgen::{
            BasicConstraints, CertificateParams, DistinguishedName, DnType,
            ExtendedKeyUsagePurpose, Ia5String, IsCa, KeyPair, KeyUsagePurpose, SanType,
        };
        use time::{Duration, OffsetDateTime};

        let now = OffsetDateTime::now_utc();
        let not_after = now + Duration::days(365 * 10);
        let san: Vec<SanType> = SAN_DOMAINS
            .iter()
            .map(|d| SanType::DnsName(Ia5String::try_from(*d).expect("valid IA5 domain")))
            .collect();

        // 证书链必须由「CA + 叶」两本证书组成：现代 webpki/rustls 会拒绝把带
        // CA 基本约束的证书当作叶证书使用（CaUsedAsEndEntity）。
        let mut ca_params = CertificateParams::default();
        let mut ca_dn = DistinguishedName::new();
        ca_dn.push(DnType::CommonName, CA_CN);
        ca_params.distinguished_name = ca_dn;
        ca_params.subject_alt_names = san.clone();
        ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        ca_params.key_usages = vec![
            KeyUsagePurpose::KeyCertSign,
            KeyUsagePurpose::CrlSign,
            KeyUsagePurpose::DigitalSignature,
        ];
        ca_params.not_before = now;
        ca_params.not_after = not_after;

        let mut leaf_params = CertificateParams::default();
        let mut leaf_dn = DistinguishedName::new();
        leaf_dn.push(DnType::CommonName, CERT_CN);
        leaf_params.distinguished_name = leaf_dn;
        leaf_params.subject_alt_names = san;
        leaf_params.is_ca = IsCa::NoCa;
        leaf_params.key_usages = vec![
            KeyUsagePurpose::DigitalSignature,
            KeyUsagePurpose::KeyEncipherment,
        ];
        leaf_params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
        leaf_params.not_before = now;
        leaf_params.not_after = not_after;

        let key_pair = KeyPair::generate()
            .map_err(|e| AppError::Config(format!("生成 TLS 密钥对失败: {e}")))?;

        let ca_cert = ca_params
            .self_signed(&key_pair)
            .map_err(|e| AppError::Config(format!("生成 TLS CA 证书失败: {e}")))?;

        let leaf_key = KeyPair::generate()
            .map_err(|e| AppError::Config(format!("生成 TLS 密钥对失败: {e}")))?;
        let leaf_cert = leaf_params
            .signed_by(&leaf_key, &ca_cert, &key_pair)
            .map_err(|e| AppError::Config(format!("生成 TLS 叶证书失败: {e}")))?;

        if let Some(parent) = cert_path.parent() {
            fs::create_dir_all(parent).map_err(|e| AppError::io(parent, e))?;
        }

        atomic_write(cert_path, ca_cert.pem().as_bytes())?;
        atomic_write(&self.leaf_cert_path(), leaf_cert.pem().as_bytes())?;
        atomic_write(key_path, leaf_key.serialize_pem().as_bytes())?;

        Ok(())
    }

    pub fn server_config(&self) -> Result<rustls::ServerConfig, AppError> {
        use rustls::pki_types::{CertificateDer, PrivateKeyDer};

        self.ensure_cert()?;

        let leaf_pem = fs::read_to_string(self.leaf_cert_path())
            .map_err(|e| AppError::io(self.leaf_cert_path(), e))?;
        let ca_pem =
            fs::read_to_string(self.cert_path()).map_err(|e| AppError::io(self.cert_path(), e))?;
        let chain = vec![
            CertificateDer::from(pem_to_der(&leaf_pem)),
            CertificateDer::from(pem_to_der(&ca_pem)),
        ];

        let key_pem =
            fs::read_to_string(self.key_path()).map_err(|e| AppError::io(self.key_path(), e))?;
        let key_der = pem_to_der(&key_pem);
        let key = PrivateKeyDer::try_from(key_der)
            .map_err(|e| AppError::Config(format!("解析私钥失败: {e}")))?;

        rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(chain, key)
            .map_err(|e| AppError::Config(format!("创建 TLS 配置失败: {e}")))
    }
}

impl Default for TlsCertManager {
    fn default() -> Self {
        Self::new()
    }
}

pub(crate) fn pem_to_der(pem: &str) -> Vec<u8> {
    use base64::Engine;
    let b64: String = pem.lines().filter(|l| !l.starts_with("-----")).collect();
    base64::engine::general_purpose::STANDARD
        .decode(b64.as_bytes())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use std::env;
    use tempfile::TempDir;

    struct TempHome {
        #[allow(dead_code)]
        dir: TempDir,
        original_test_home: Option<String>,
    }

    impl TempHome {
        fn new() -> Self {
            let dir = TempDir::new().expect("failed to create temp home");
            let original_test_home = env::var("CC_SWITCH_TEST_HOME").ok();
            env::set_var("CC_SWITCH_TEST_HOME", dir.path());
            Self {
                dir,
                original_test_home,
            }
        }
    }

    impl Drop for TempHome {
        fn drop(&mut self) {
            match &self.original_test_home {
                Some(value) => env::set_var("CC_SWITCH_TEST_HOME", value),
                None => env::remove_var("CC_SWITCH_TEST_HOME"),
            }
        }
    }

    #[test]
    #[serial]
    fn ensure_cert_generates_files_on_empty_dir() {
        let _home = TempHome::new();

        let manager = TlsCertManager::new();
        let (cert_path, key_path) = manager.ensure_cert().expect("ensure_cert should succeed");

        assert!(cert_path.exists(), "cert file should exist");
        assert!(key_path.exists(), "key file should exist");

        let cert_content = std::fs::read_to_string(&cert_path).expect("read cert");
        assert!(
            cert_content.contains("BEGIN CERTIFICATE"),
            "cert file should contain PEM certificate"
        );

        let key_content = std::fs::read_to_string(&key_path).expect("read key");
        assert!(
            key_content.contains("BEGIN PRIVATE KEY")
                || key_content.contains("BEGIN EC PRIVATE KEY"),
            "key file should contain PEM private key"
        );
    }

    #[test]
    #[serial]
    fn ensure_cert_is_idempotent() {
        let _home = TempHome::new();

        let manager = TlsCertManager::new();
        let (cert_path1, key_path1) = manager.ensure_cert().expect("first ensure_cert");

        let cert_content1 = std::fs::read_to_string(&cert_path1).expect("read cert 1");
        let key_content1 = std::fs::read_to_string(&key_path1).expect("read key 1");

        let (cert_path2, key_path2) = manager.ensure_cert().expect("second ensure_cert");

        assert_eq!(cert_path1, cert_path2, "cert path should be the same");
        assert_eq!(key_path1, key_path2, "key path should be the same");

        let cert_content2 = std::fs::read_to_string(&cert_path2).expect("read cert 2");
        let key_content2 = std::fs::read_to_string(&key_path2).expect("read key 2");

        assert_eq!(
            cert_content1, cert_content2,
            "cert content should not change on second call"
        );
        assert_eq!(
            key_content1, key_content2,
            "key content should not change on second call"
        );
    }

    #[test]
    #[serial]
    fn generated_cert_contains_correct_san_entries() {
        let _home = TempHome::new();

        let manager = TlsCertManager::new();
        let (cert_path, _) = manager.ensure_cert().expect("ensure_cert");

        let cert_pem = std::fs::read_to_string(&cert_path).expect("read cert pem");
        let der = pem_to_der(&cert_pem);

        for domain in SAN_DOMAINS {
            assert!(
                der.windows(domain.len()).any(|w| w == domain.as_bytes()),
                "cert DER should contain SAN entry: {domain}"
            );
        }
    }
}
