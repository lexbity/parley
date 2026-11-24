//! Immutable owner provider preferences and the qualified activation pointer.
use serde::{Deserialize, Serialize};
use sqlx::{Row, SqliteConnection};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ObservedPairing {
    pub(crate) provider_version: String,
    pub(crate) model_digest: String,
    pub(crate) tokenizer_digest: String,
    pub(crate) template_tokens: u32,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ProfileConfiguration {
    pub(crate) endpoint: String,
    pub(crate) model: String,
    pub(crate) num_ctx: i64,
    pub(crate) num_predict: i64,
    pub(crate) locale: String,
    pub(crate) pairing: Option<ObservedPairing>,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct ProfileSettings {
    pub(crate) revision: i64,
    pub(crate) active_revision: Option<i64>,
    pub(crate) endpoint: Option<String>,
    pub(crate) model: Option<String>,
    pub(crate) num_ctx: i64,
    pub(crate) num_predict: i64,
    pub(crate) locale: String,
    pub(crate) pairing: Option<ObservedPairing>,
    pub(crate) diagnostic: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum ProfileError {
    #[error("provider settings changed")]
    Conflict,
    #[error("provider settings field {0} is invalid")]
    Invalid(&'static str),
    #[error("stored provider settings are inconsistent")]
    Inconsistent,
    #[error("provider settings storage is unavailable")]
    Storage(#[from] sqlx::Error),
}

pub(crate) async fn get_settings(
    connection: &mut SqliteConnection,
) -> Result<Option<ProfileSettings>, ProfileError> {
    let row = sqlx::query(
        "SELECT p.revision,p.endpoint,p.chat_model,p.context_tokens,p.output_tokens,p.locale,
                p.provider_version,p.chat_model_digest,p.tokenizer_digest,
                json_extract(p.recipe_json,'$.template_tokens') AS template_tokens,
                a.profile_revision AS active_revision,r.diagnostic
         FROM inference_profile p
         LEFT JOIN inference_active_profile a ON a.singleton=1
         LEFT JOIN inference_profile_revocation r ON r.profile_revision=p.revision
         ORDER BY p.revision DESC LIMIT 1",
    )
    .fetch_optional(connection)
    .await?;
    let Some(row) = row else {
        return Ok(None);
    };
    let pairing = match (
        row.try_get("provider_version")?,
        row.try_get("chat_model_digest")?,
        row.try_get("tokenizer_digest")?,
        row.try_get::<Option<u32>, _>("template_tokens")?,
    ) {
        (
            Some(provider_version),
            Some(model_digest),
            Some(tokenizer_digest),
            Some(template_tokens),
        ) => Some(ObservedPairing {
            provider_version,
            model_digest,
            tokenizer_digest,
            template_tokens,
        }),
        (None, None, None, None) => None,
        _ => return Err(ProfileError::Inconsistent),
    };
    Ok(Some(ProfileSettings {
        revision: row.try_get("revision")?,
        active_revision: row.try_get("active_revision")?,
        endpoint: row.try_get("endpoint")?,
        model: row.try_get("chat_model")?,
        num_ctx: row.try_get("context_tokens")?,
        num_predict: row.try_get("output_tokens")?,
        locale: row.try_get("locale")?,
        pairing,
        diagnostic: row.try_get("diagnostic")?,
    }))
}

/// Insert a new immutable candidate within the caller's writer reservation.
/// Observation identities describe preferences until the independent
/// qualification and activation operations record evidence for this revision.
pub(crate) async fn replace_settings(
    connection: &mut SqliteConnection,
    expected_revision: i64,
    input: ProfileConfiguration,
    runner_build_digest: Option<&str>,
) -> Result<ProfileSettings, ProfileError> {
    let latest: i64 = sqlx::query_scalar("SELECT COALESCE(MAX(revision),0) FROM inference_profile")
        .fetch_one(&mut *connection)
        .await?;
    if expected_revision != latest {
        return Err(ProfileError::Conflict);
    }
    validate(&input)?;
    if runner_build_digest.is_some_and(|digest| !valid_digest(digest)) {
        return Err(ProfileError::Inconsistent);
    }

    let pairing = input.pairing.as_ref();
    let recipe = pairing.map_or_else(
        || "{}".into(),
        |pairing| serde_json::json!({ "template_tokens": pairing.template_tokens }).to_string(),
    );
    let revision = latest.checked_add(1).ok_or(ProfileError::Inconsistent)?;
    sqlx::query(
        "INSERT INTO inference_profile(
           revision,runner_kind,runner_build_digest,protocol_major,provider_version,
           endpoint,chat_model,chat_model_digest,embed_model,embed_model_digest,
           tokenizer_digest,recipe_json,context_tokens,output_tokens,locale,created_at
         )
         VALUES (?, 'pi', ?, 1, ?, ?, ?, ?,
             (SELECT embed_model FROM inference_profile WHERE revision=?),
             (SELECT embed_model_digest FROM inference_profile WHERE revision=?),
             ?, ?, ?, ?, ?, ?)",
    )
    .bind(revision)
    .bind(runner_build_digest)
    .bind(pairing.map(|pairing| pairing.provider_version.as_str()))
    .bind(input.endpoint)
    .bind((!input.model.is_empty()).then_some(input.model))
    .bind(pairing.map(|pairing| pairing.model_digest.as_str()))
    .bind(latest)
    .bind(latest)
    .bind(pairing.map(|pairing| pairing.tokenizer_digest.as_str()))
    .bind(recipe)
    .bind(input.num_ctx)
    .bind(input.num_predict)
    .bind(input.locale)
    .bind(crate::now_utc())
    .execute(&mut *connection)
    .await?;
    get_settings(connection)
        .await?
        .ok_or(ProfileError::Inconsistent)
}

/// Parse the one configured upstream authority accepted by a profile.
pub(crate) fn loopback_authority(endpoint: &str) -> Result<SocketAddr, ProfileError> {
    endpoint
        .strip_prefix("http://")
        .and_then(|address| address.parse::<SocketAddr>().ok())
        .filter(|address| {
            address.port() > 0
                && matches!(
                    address.ip(),
                    IpAddr::V4(Ipv4Addr::LOCALHOST) | IpAddr::V6(Ipv6Addr::LOCALHOST)
                )
                && endpoint == format!("http://{address}")
        })
        .ok_or(ProfileError::Invalid("endpoint"))
}

fn validate(input: &ProfileConfiguration) -> Result<(), ProfileError> {
    loopback_authority(&input.endpoint)?;
    if input.model.len() > 192
        || input.model.bytes().any(|byte| {
            !byte.is_ascii_alphanumeric() && !matches!(byte, b'_' | b'-' | b'.' | b'/' | b':')
        })
        || input.model.contains("..")
        || input.model.starts_with('/')
        || input.model.ends_with('/')
        || input
            .model
            .split('/')
            .any(|part| part.is_empty() && !input.model.is_empty())
        || input
            .model
            .to_ascii_lowercase()
            .split(':')
            .any(|part| part == "cloud" || part.ends_with("-cloud"))
    {
        return Err(ProfileError::Invalid("model"));
    }
    if !(1024..=131_072).contains(&input.num_ctx) {
        return Err(ProfileError::Invalid("num_ctx"));
    }
    let context_reserve = (input.num_ctx + 9) / 10;
    if !(64..=8192).contains(&input.num_predict)
        || input.num_predict >= input.num_ctx - context_reserve
    {
        return Err(ProfileError::Invalid("num_predict"));
    }
    if input.locale.is_empty()
        || input.locale.len() > 32
        || input
            .locale
            .split('-')
            .any(|part| part.is_empty() || !part.bytes().all(|byte| byte.is_ascii_alphanumeric()))
    {
        return Err(ProfileError::Invalid("locale"));
    }
    if let Some(pairing) = &input.pairing {
        if input.model.is_empty() {
            return Err(ProfileError::Invalid("model"));
        }
        if pairing.provider_version.is_empty()
            || pairing.provider_version.len() > 64
            || !pairing
                .provider_version
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'+' | b'-'))
        {
            return Err(ProfileError::Invalid("provider_version"));
        }
        if !valid_digest(&pairing.model_digest) {
            return Err(ProfileError::Invalid("model_digest"));
        }
        if !valid_digest(&pairing.tokenizer_digest) {
            return Err(ProfileError::Invalid("tokenizer_digest"));
        }
        if i64::from(pairing.template_tokens) >= input.num_ctx - input.num_predict - context_reserve
        {
            return Err(ProfileError::Invalid("template_tokens"));
        }
    }
    Ok(())
}

fn valid_digest(digest: &str) -> bool {
    if digest.len() != 64 {
        return false;
    }
    digest
        .bytes()
        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::Connection;

    #[test]
    fn ar01_profile_accepts_only_canonical_loopback_authorities() {
        for endpoint in ["http://127.0.0.1:11434", "http://[::1]:11434"] {
            assert!(loopback_authority(endpoint).is_ok());
        }
        for endpoint in [
            "http://localhost:11434",
            "http://127.0.0.1",
            "http://127.0.0.1:011434",
            "http://127.0.0.1:11434/",
            "http://127.0.0.1:11434#profile",
            "http://127.0.0.1:11434?model=x",
            "http://0x7f000001:11434",
            "http://127.0.0.2:11434",
            "http://[::ffff:127.0.0.1]:11434",
            "http://owner@127.0.0.1:11434",
            "https://127.0.0.1:11434",
        ] {
            assert!(loopback_authority(endpoint).is_err(), "{endpoint}");
        }
    }

    #[tokio::test]
    async fn ar01_profile_settings_cas_does_not_activate_an_observed_pairing() {
        let mut db = SqliteConnection::connect("sqlite::memory:").await.unwrap();
        sqlx::migrate!("../../migrations")
            .run(&mut db)
            .await
            .unwrap();

        let configuration = || ProfileConfiguration {
            endpoint: "http://[::1]:11434".into(),
            model: "local:approved".into(),
            num_ctx: 8192,
            num_predict: 1024,
            locale: "en-CA".into(),
            pairing: Some(ObservedPairing {
                provider_version: "0.18.0".into(),
                model_digest: "1".repeat(64),
                tokenizer_digest: "2".repeat(64),
                template_tokens: 256,
            }),
        };
        let installed = "3".repeat(64);
        let mut tx = db.begin_with("BEGIN IMMEDIATE").await.unwrap();
        let settings = replace_settings(&mut tx, 1, configuration(), Some(&installed))
            .await
            .unwrap();
        assert_eq!(settings.revision, 2);
        assert_eq!(settings.active_revision, None);
        assert_eq!(settings.pairing.unwrap().template_tokens, 256);
        assert!(matches!(
            replace_settings(&mut tx, 1, configuration(), Some(&installed)).await,
            Err(ProfileError::Conflict)
        ));

        let mut cloud = configuration();
        cloud.model = "remote:480b-cloud".into();
        assert!(matches!(
            replace_settings(&mut tx, 2, cloud, Some(&installed)).await,
            Err(ProfileError::Invalid("model"))
        ));
        tx.commit().await.unwrap();
        assert_eq!(get_settings(&mut db).await.unwrap().unwrap().revision, 2);
    }
}
