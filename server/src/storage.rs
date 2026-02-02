use std::path::PathBuf;

use crate::state::PersistentSessionData;
use async_trait::async_trait;
use aws_config::BehaviorVersion;
use aws_sdk_s3::primitives::ByteStream;
use aws_sdk_s3::Client;

#[async_trait]
pub trait Storage: Send + Sync {
    async fn load_session(&self, session_id: &str) -> Option<PersistentSessionData>;
    async fn save_session(&self, session_id: &str, data: &PersistentSessionData);
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
    async fn load_session(&self, session_id: &str) -> Option<PersistentSessionData> {
        let path = self.session_dir.join(format!("{session_id}.bin"));
        let payload = tokio::fs::read(path).await.ok()?;
        decode_data(&payload)
    }

    async fn save_session(&self, session_id: &str, data: &PersistentSessionData) {
        let path = self.session_dir.join(format!("{session_id}.bin"));
        let payload = encode_data(data);
        if let Err(error) = tokio::fs::write(path, payload).await {
            eprintln!("Failed to save session {session_id}: {error}");
        }
    }
}

fn encode_data(data: &PersistentSessionData) -> Vec<u8> {
    bincode::encode_to_vec(data, bincode::config::standard()).unwrap_or_default()
}

fn decode_data(payload: &[u8]) -> Option<PersistentSessionData> {
    bincode::decode_from_slice(payload, bincode::config::standard())
        .map(|(data, _)| data)
        .ok()
}

#[derive(Clone, Debug)]
#[allow(dead_code)]
pub struct S3StorageConfig {
    pub bucket: String,
    pub prefix: Option<String>,
    pub region: Option<String>,
    pub endpoint_url: Option<String>,
    pub force_path_style: bool,
}

#[allow(dead_code)]
impl S3StorageConfig {
    pub fn new(bucket: impl Into<String>) -> Self {
        Self {
            bucket: bucket.into(),
            prefix: None,
            region: None,
            endpoint_url: None,
            force_path_style: false,
        }
    }
}

#[allow(dead_code)]
pub struct S3Storage {
    bucket: String,
    prefix: String,
    client: Client,
}

#[allow(dead_code)]
impl S3Storage {
    pub async fn new(config: S3StorageConfig) -> Self {
        let mut loader = aws_config::defaults(BehaviorVersion::latest());
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
        let client = Client::from_conf(builder.build());
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
            format!("{session_id}.bin")
        } else {
            format!("{}/{session_id}.bin", self.prefix)
        }
    }
}

#[async_trait]
impl Storage for S3Storage {
    async fn load_session(&self, session_id: &str) -> Option<PersistentSessionData> {
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
                        return None;
                    }
                }
                eprintln!("Failed to load session {session_id} from s3: {error:?}");
                return None;
            }
        };
        let bytes = match output.body.collect().await {
            Ok(collected) => collected.into_bytes(),
            Err(error) => {
                eprintln!("Failed to read session {session_id} from s3 response: {error:?}");
                return None;
            }
        };
        decode_data(&bytes)
    }

    async fn save_session(&self, session_id: &str, data: &PersistentSessionData) {
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
            eprintln!("Failed to save session {session_id} to s3: {error:?}");
        }
    }
}
