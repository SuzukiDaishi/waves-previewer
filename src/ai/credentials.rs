use secrecy::SecretString;
use zeroize::Zeroize;

use super::provider::AiError;

const SERVICE: &str = "NeoWaves";
const ACCOUNT: &str = "gemini-api-key";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CredentialSource {
    Environment,
    Keyring,
}

pub fn load_api_key() -> Result<(SecretString, CredentialSource), AiError> {
    if let Ok(mut value) = std::env::var("GEMINI_API_KEY") {
        let trimmed = value.trim().to_string();
        value.zeroize();
        if !trimmed.is_empty() {
            return Ok((
                SecretString::new(trimmed.into_boxed_str()),
                CredentialSource::Environment,
            ));
        }
    }

    let entry = keyring::Entry::new(SERVICE, ACCOUNT).map_err(|error| {
        AiError::Storage(format!("could not open OS credential store: {error}"))
    })?;
    let mut value = entry.get_password().map_err(|_| AiError::MissingApiKey)?;
    let trimmed = value.trim().to_string();
    value.zeroize();
    if trimmed.is_empty() {
        return Err(AiError::MissingApiKey);
    }
    Ok((
        SecretString::new(trimmed.into_boxed_str()),
        CredentialSource::Keyring,
    ))
}

pub fn api_key_is_configured() -> bool {
    load_api_key().is_ok()
}

pub fn save_api_key(value: &str) -> Result<(), AiError> {
    let value = value.trim();
    if value.is_empty() {
        return Err(AiError::MissingApiKey);
    }
    let entry = keyring::Entry::new(SERVICE, ACCOUNT).map_err(|error| {
        AiError::Storage(format!("could not open OS credential store: {error}"))
    })?;
    entry
        .set_password(value)
        .map_err(|error| AiError::Storage(format!("could not save API key: {error}")))
}

pub fn delete_stored_api_key() -> Result<(), AiError> {
    let entry = keyring::Entry::new(SERVICE, ACCOUNT).map_err(|error| {
        AiError::Storage(format!("could not open OS credential store: {error}"))
    })?;
    entry
        .delete_credential()
        .map_err(|error| AiError::Storage(format!("could not delete API key: {error}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use secrecy::ExposeSecret;

    struct RemoveDiagnosticCredential;

    impl Drop for RemoveDiagnosticCredential {
        fn drop(&mut self) {
            let _ = delete_stored_api_key();
        }
    }

    #[test]
    #[ignore = "touches the current user's OS credential store; run explicitly"]
    fn os_credential_store_roundtrip() {
        const DIAGNOSTIC_VALUE: &str = "neowaves-keyring-diagnostic-not-an-api-key";
        let _cleanup = RemoveDiagnosticCredential;
        save_api_key(DIAGNOSTIC_VALUE).expect("save diagnostic credential");
        let (loaded, source) = load_api_key().expect("read diagnostic credential back");
        assert_eq!(source, CredentialSource::Keyring);
        assert_eq!(loaded.expose_secret(), DIAGNOSTIC_VALUE);
    }
}
