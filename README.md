# GoRules Agent

[Website](https://gorules.io) | [Documentation](https://docs.gorules.io) | [GitHub - public mirror](https://github.com/gorules/agent-public)

The GoRules Agent is an Open-source, standalone microservice that acts as a high-performance Rules Engine over REST, without requiring a UI. It is designed to pull Releases from Object Storage, automatically re-load them at runtime when changes occur, and evaluate decision models efficiently. This ensures that your rules are always up-to-date and accessible with minimal configuration.

## Environment Variables

### AWS

In case your deployment supports IAM, most of the environment variables below are __optional__.

```bash
PROVIDER__TYPE=S3
PROVIDER__BUCKET=bucket
PROVIDER__REGION=us-east-1 # Optional in case of IAM
AWS_ACCESS_KEY_ID=<aws-access-key-id> # Optional in case of IAM
AWS_SECRET_ACCESS_KEY=<aws-secret-access-key> # Optional in case of IAM
```

### Azure

```bash
PROVIDER__TYPE=AzureStorage
PROVIDER__CONNECTION_STRING=<connection-string>
PROVIDER__CONTAINER=<container-name>
```

### Google Cloud

```bash
PROVIDER__TYPE=GCS
PROVIDER__BUCKET=<bucket-name>
PROVIDER__BASE64_CONTENTS=<base64-credential-contents>
```

### FileSystem
```bash
PROVIDER__TYPE=Filesystem
```

### FileSystem Zip
For FileSystem type, all project zips should be in ./data folder.
You can build your own image bundled with rules by doing docker build from our image and adding layer that adds ./data folder
```bash
PROVIDER__TYPE=Zip
```

### MinIO
```bash
PROVIDER__TYPE=S3
PROVIDER__REGION=us-east-1
PROVIDER__BUCKET=bucket
PROVIDER__FORCE_PATH_STYLE=true
PROVIDER__ENDPOINT=http://localhost:9000
PROVIDER__PREFIX=folder/
AWS_ACCESS_KEY_ID=
AWS_SECRET_ACCESS_KEY=
```