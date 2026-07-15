# Azure IAM v2 — Minimal, via existing SDK's DefaultAzureCredential

**Date:** 2026-07-10
**Status:** Approved (supersedes 2026-07-09 hybrid design / PR #69 for now; PR #69 remains
the reference for a future official-SDK migration)
**Branch:** `feat/grl-547-azure-iam-provider-default-cred` (from master v1.25.6)

## Decision

Promote the existing `DefaultAzureCredential` fallback (shipped as GRL-204) to a
first-class, documented option — using only the already-present community SDK 0.21 crates.
No new dependencies, no new provider, no custom credential chain.

Trade-offs knowingly accepted: IAM rides on the deprecated community crate;
0.21's `DefaultAzureCredential` does not report which credential source won; whether
`AZURE_CLIENT_ID` selects a user-assigned managed identity is verified in E2E, not code.

## Changes

1. **Config** (`src/config.rs`): `AzureStorageProviderConfig.connection_string` becomes
   `Option<String>`; new `account_name: Option<String>`.
2. **Auth resolution** (`src/provider/azure_storage.rs`): extracted, unit-tested
   `resolve_auth(config) -> anyhow::Result<AzureAuth>` where
   `enum AzureAuth { ConnectionString(String), Iam { account_name: String } }`.
   Empty/whitespace-only values are treated as unset. Rules:
   - connection string with `AccountKey` **or SAS** → `ConnectionString` (SAS starts
     working — the 0.21 SDK's `storage_credentials()` supports it; previously SAS-only
     strings fell into the token branch and failed confusingly)
   - `account_name` set → `Iam`
   - keyless connection string with `AccountName` → `Iam` + warn log suggesting
     `PROVIDER__ACCOUNT_NAME` (GRL-204 compat, unchanged behavior)
   - both set / neither set / keyless without `AccountName` → clear startup errors naming
     the env vars
3. **Constructor**: `Iam` → `StorageCredentials::token_credential(DefaultAzureCredential)`
   (the code that exists today); `ConnectionString` → `storage_credentials()`.
4. **Error hint**: listing failures in `load_data` get context noting the identity needs
   the "Storage Blob Data Reader" role.
5. **Docs**: `.env.example` gains the IAM block; CLAUDE.md line reworded (stays untracked).

## Testing

Unit tests for `resolve_auth` only (~9 cases). Runtime behavior via manual E2E
(`docs/superpowers/plans/2026-07-10-azure-iam-e2e.md`), scenarios unchanged except the
provider under test is the single legacy provider.
