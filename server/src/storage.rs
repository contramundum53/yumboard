use std::path::PathBuf;

use crate::state::PersistentSessionData;
use async_trait::async_trait;
use aws_config::BehaviorVersion;
use aws_credential_types::Credentials;
use aws_sdk_s3::primitives::ByteStream;
use aws_sdk_s3::Client;
use yumboard_shared::{
    decode_session_file, encode_session_file, SessionFileData, SessionFileDecodeError,
};

#[derive(Debug)]
pub enum StorageError {
    NotFound,
    Other(String),
}

impl std::fmt::Display for StorageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StorageError::NotFound => write!(f, "session-not-found"),
            StorageError::Other(message) => write!(f, "{message}"),
        }
    }
}

#[async_trait]
pub trait Storage: Send + Sync {
    async fn load_session(&self, session_id: &str) -> Result<PersistentSessionData, StorageError>;
    async fn save_session(
        &self,
        session_id: &str,
        data: &PersistentSessionData,
    ) -> Result<(), String>;
}

pub struct FileStorage {
    session_dir: PathBuf,
}

impl FileStorage {
    pub fn new(session_dir: PathBuf) -> Self {
        Self { session_dir }
    }
}

#[async_trait]
impl Storage for FileStorage {
    async fn load_session(&self, session_id: &str) -> Result<PersistentSessionData, StorageError> {
        let path = self.session_dir.join(format!("{session_id}.ybss"));
        let payload = tokio::fs::read(path).await.map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                StorageError::NotFound
            } else {
                StorageError::Other(format!("Failed to read session file for {session_id}: {e}"))
            }
        })?;
        decode_data(&payload).map_err(StorageError::Other)
    }

    async fn save_session(
        &self,
        session_id: &str,
        data: &PersistentSessionData,
    ) -> Result<(), String> {
        let path = self.session_dir.join(format!("{session_id}.ybss"));
        let payload = encode_data(data);
        if let Err(error) = tokio::fs::write(path, payload).await {
            return Err(format!("Failed to save session {session_id}: {error}"));
        }
        Ok(())
    }
}

fn encode_data(data: &PersistentSessionData) -> Vec<u8> {
    let file = SessionFileData {
        strokes: data.strokes.clone(),
    };
    encode_session_file(&file)
}

fn decode_data(payload: &[u8]) -> Result<PersistentSessionData, String> {
    match decode_session_file(payload) {
        Ok(data) => Ok(PersistentSessionData {
            strokes: data.strokes,
        }),
        Err(SessionFileDecodeError::UnsupportedVersion(version)) => {
            Err(format!("Unsupported session file version: {version}"))
        }
        Err(SessionFileDecodeError::InvalidData) => Err("Invalid session file format".into()),
    }
}

#[derive(Clone, Debug)]
pub struct S3StorageConfig {
    pub bucket: String,
    pub prefix: Option<String>,
    pub region: Option<String>,
    pub endpoint_url: Option<String>,
    pub force_path_style: bool,
    pub access_key_id: Option<String>,
    pub secret_access_key: Option<String>,
}

impl S3StorageConfig {
    pub fn new(bucket: impl Into<String>) -> Self {
        Self {
            bucket: bucket.into(),
            prefix: None,
            region: None,
            endpoint_url: None,
            force_path_style: false,
            access_key_id: None,
            secret_access_key: None,
        }
    }
}

pub struct S3Storage {
    bucket: String,
    prefix: String,
    client: Client,
}

impl S3Storage {
    pub async fn new(config: S3StorageConfig) -> Self {
        let mut loader = aws_config::defaults(BehaviorVersion::latest());
        if let (Some(access_key_id), Some(secret_access_key)) = (
            config.access_key_id.clone(),
            config.secret_access_key.clone(),
        ) {
            let creds = Credentials::new(access_key_id, secret_access_key, None, None, "static");
            loader = loader.credentials_provider(creds);
        }
        if let Some(region) = config.region.clone() {
            loader = loader.region(aws_config::Region::new(region));
        }
        let shared = loader.load().await;
        let mut builder = aws_sdk_s3::config::Builder::from(&shared);
        if let Some(endpoint_url) = config.endpoint_url.as_ref() {
            builder = builder.endpoint_url(endpoint_url);
        }
        if config.force_path_style {
            builder = builder.force_path_style(true);
        }
        let aws_config = builder.build();
        eprintln!("S3 config: {:?}", aws_config);
        let client = Client::from_conf(aws_config);
        let prefix = config
            .prefix
            .unwrap_or_default()
            .trim_matches('/')
            .to_string();
        Self {
            bucket: config.bucket,
            prefix,
            client,
        }
    }

    fn object_key(&self, session_id: &str) -> String {
        if self.prefix.is_empty() {
            format!("{session_id}.ybss")
        } else {
            format!("{}/{session_id}.ybss", self.prefix)
        }
    }
}

#[async_trait]
impl Storage for S3Storage {
    async fn load_session(&self, session_id: &str) -> Result<PersistentSessionData, StorageError> {
        let key = self.object_key(session_id);
        let response = self
            .client
            .get_object()
            .bucket(&self.bucket)
            .key(key)
            .send()
            .await;
        let output = match response {
            Ok(output) => output,
            Err(error) => {
                if let Some(service_error) = error.as_service_error() {
                    if service_error.is_no_such_key() {
                        return Err(StorageError::NotFound);
                    }
                }
                return Err(StorageError::Other(format!(
                    "Failed to load session {session_id} from s3: {error:?}"
                )));
            }
        };
        let bytes = match output.body.collect().await {
            Ok(collected) => collected.into_bytes(),
            Err(error) => {
                return Err(StorageError::Other(format!(
                    "Failed to read session {session_id} from s3 response: {error:?}"
                )));
            }
        };
        decode_data(&bytes).map_err(StorageError::Other)
    }

    async fn save_session(
        &self,
        session_id: &str,
        data: &PersistentSessionData,
    ) -> Result<(), String> {
        let key = self.object_key(session_id);
        let payload = encode_data(data);
        let body = ByteStream::from(payload);
        if let Err(error) = self
            .client
            .put_object()
            .bucket(&self.bucket)
            .key(key)
            .body(body)
            .send()
            .await
        {
            return Err(format!(
                "Failed to save session {session_id} to s3: {error:?}"
            ));
        }
        Ok(())
    }
}
