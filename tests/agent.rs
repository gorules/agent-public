use crate::support::minio::MinioContainer;
use crate::support::path::ValidateProject;
use agent::app;
use agent::config::{EnvironmentConfig, ProviderConfig, S3ProviderConfig, ZipProviderConfig};
use std::env;

mod support;

#[tokio::test]
async fn zip_agent() {
    let config = EnvironmentConfig {
        provider: ProviderConfig::Zip(ZipProviderConfig {
            root_dir: "tests/data".to_string(),
        }),
        ..Default::default()
    };

    let agent = app::create_agent(config, Default::default()).await;

    assert!(
        agent.project("sample-project").is_some(),
        "sample-project was not found"
    );
    assert!(
        agent.project("SampleProject").is_some(),
        "SampleProject was not found"
    );

    assert!(
        agent.project("nested/nested-project").is_none(),
        "nested-project was found"
    );
}

#[tokio::test]
async fn s3_agent() {
    let minio = MinioContainer::start()
        .await
        .expect("Minio container is available");
    let host_port = minio
        .container
        .get_host_port_ipv4(9000)
        .await
        .expect("Minio port 9000 is available");
    let minio_image = minio.container.image();

    unsafe { env::set_var("AWS_ACCESS_KEY_ID", minio_image.username.clone()) };
    unsafe { env::set_var("AWS_SECRET_ACCESS_KEY", minio_image.password.clone()) };

    let config = EnvironmentConfig {
        provider: ProviderConfig::S3(S3ProviderConfig {
            bucket: minio_image.bucket_name.to_string(),
            endpoint: Some(minio_image.endpoint(host_port)),
            prefix: None,
            force_path_style: true,
        }),
        ..Default::default()
    };

    let agent = app::create_agent(config, Default::default()).await;

    assert!(
        agent.project("sample-project").is_some(),
        "sample-project was not found"
    );
    assert!(
        agent.project("SampleProject").is_some(),
        "SampleProject was not found"
    );

    assert!(
        agent.project("nested/nested-project").is_none(),
        "nested-project was found"
    );

    let sample_project = agent
        .project("sample-project")
        .expect("sample-project was not found");
    sample_project.validate_project().await;
}

#[tokio::test]
async fn s3_agent_prefix() {
    let minio = MinioContainer::start()
        .await
        .expect("Minio container is available");
    let host_port = minio
        .container
        .get_host_port_ipv4(9000)
        .await
        .expect("Minio port 9000 is available");
    let minio_image = minio.container.image();

    unsafe { env::set_var("AWS_ACCESS_KEY_ID", minio_image.username.clone()) };
    unsafe { env::set_var("AWS_SECRET_ACCESS_KEY", minio_image.password.clone()) };

    let config = EnvironmentConfig {
        provider: ProviderConfig::S3(S3ProviderConfig {
            bucket: minio_image.bucket_name.to_string(),
            prefix: Some("nested".to_string()),
            endpoint: Some(minio_image.endpoint(host_port)),
            force_path_style: true,
        }),
        ..Default::default()
    };

    let agent = app::create_agent(config, Default::default()).await;
    assert!(
        agent.project("nested-project").is_some(),
        "nested-project was not found"
    );

    let nested_project = agent
        .project("nested-project")
        .expect("nested-project was not found");
    nested_project.validate_project().await;
}
