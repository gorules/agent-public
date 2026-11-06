use crate::support::path::data_path_files;
use aws_config::BehaviorVersion;
use aws_config::meta::region::RegionProviderChain;
use aws_sdk_s3::Client;
use aws_sdk_s3::config::Credentials;
use aws_sdk_s3::primitives::ByteStream;
use std::borrow::Cow;
use std::collections::HashMap;
use std::time::Duration;
use testcontainers::core::{ContainerPort, WaitFor};
use testcontainers::runners::AsyncRunner;
use testcontainers::{ContainerAsync, Image};

pub struct S3Container {
    pub container: ContainerAsync<S3Image>,
    #[allow(dead_code)]
    pub client: Client,
}

impl S3Container {
    pub async fn start() -> Result<Self, Box<dyn std::error::Error + 'static>> {
        let container = S3Image::default().start().await?;
        let host_port = container.get_host_port_ipv4(9000).await?;
        let client = container.image().s3_client(host_port).await;

        let bucket = container.image().bucket_name.as_str();

        // RustFS prints nothing once it is listening, so retry the first call
        // until the API answers.
        let mut attempts = 0;
        while let Err(err) = client.create_bucket().bucket(bucket).send().await {
            attempts += 1;
            if attempts >= 50 {
                return Err(err.into());
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }

        for pf in data_path_files() {
            let buf_data = pf.read();

            client
                .put_object()
                .bucket(bucket)
                .key(pf.relative_path.strip_suffix(".zip").unwrap())
                .body(ByteStream::from(buf_data))
                .send()
                .await?;
        }

        Ok(Self { container, client })
    }
}

#[derive(Debug)]
pub struct S3Image {
    pub username: String,
    pub password: String,
    pub bucket_name: String,
}

impl Default for S3Image {
    fn default() -> Self {
        let bucket_name = "sample-bucket".to_string();

        Self {
            username: "s3-username".to_string(),
            password: "s3-password".to_string(),
            bucket_name,
        }
    }
}

impl S3Image {
    pub fn endpoint(&self, host_port: u16) -> String {
        format!("http://127.0.0.1:{host_port}")
    }

    async fn s3_client(&self, host_port: u16) -> Client {
        let endpoint_uri = self.endpoint(host_port);
        let region_provider = RegionProviderChain::default_provider().or_else("us-east-1");
        let creds = Credentials::new(&self.username, &self.password, None, None, "test");

        // Credentials the container is started with, see env_vars
        let shared_config = aws_config::defaults(BehaviorVersion::latest())
            .region(region_provider)
            .endpoint_url(endpoint_uri)
            .credentials_provider(creds)
            .load()
            .await;

        Client::new(&shared_config)
    }
}

impl Image for S3Image {
    fn name(&self) -> &str {
        "rustfs/rustfs"
    }

    fn tag(&self) -> &str {
        "1.0.0"
    }

    fn ready_conditions(&self) -> Vec<WaitFor> {
        vec![WaitFor::message_on_stdout("Starting:")]
    }

    fn env_vars(
        &self,
    ) -> impl IntoIterator<Item = (impl Into<Cow<'_, str>>, impl Into<Cow<'_, str>>)> {
        let mut variables = HashMap::new();
        variables.insert("RUSTFS_ACCESS_KEY", &self.username);
        variables.insert("RUSTFS_SECRET_KEY", &self.password);

        variables
    }

    fn expose_ports(&self) -> &[ContainerPort] {
        &[ContainerPort::Tcp(9000)]
    }
}
