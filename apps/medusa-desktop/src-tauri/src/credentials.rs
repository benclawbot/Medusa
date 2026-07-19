use keyring::v1::{Entry, Error};

const CREDENTIAL_SERVICE: &str = "com.benclawbot.medusa";

pub trait CredentialStore {
    fn load(&self, provider: &str) -> Result<Option<String>, String>;
    fn save(&self, provider: &str, api_key: &str) -> Result<(), String>;
}

pub struct SystemCredentialStore;

impl CredentialStore for SystemCredentialStore {
    fn load(&self, provider: &str) -> Result<Option<String>, String> {
        let entry = entry(provider)?;
        match entry.get_password() {
            Ok(api_key) => Ok(Some(api_key)),
            Err(Error::NoEntry | Error::NoDefaultStore) => Ok(None),
            Err(error) => Err(format!("cannot read the saved {provider} API key: {error}")),
        }
    }

    fn save(&self, provider: &str, api_key: &str) -> Result<(), String> {
        entry(provider)?
            .set_password(api_key)
            .map_err(|error| format!("cannot save the {provider} API key: {error}"))
    }
}

fn entry(provider: &str) -> Result<Entry, String> {
    let account = provider.trim().to_ascii_lowercase();
    if account.is_empty() {
        return Err("provider is required before storing an API key".to_owned());
    }
    Entry::new(CREDENTIAL_SERVICE, &account)
        .map_err(|error| format!("cannot open the operating system credential store: {error}"))
}

#[cfg(test)]
mod tests {
    use std::{cell::RefCell, collections::BTreeMap};

    use super::*;

    #[derive(Default)]
    struct MemoryCredentialStore {
        values: RefCell<BTreeMap<String, String>>,
    }

    impl CredentialStore for MemoryCredentialStore {
        fn load(&self, provider: &str) -> Result<Option<String>, String> {
            Ok(self.values.borrow().get(provider).cloned())
        }

        fn save(&self, provider: &str, api_key: &str) -> Result<(), String> {
            self.values
                .borrow_mut()
                .insert(provider.to_owned(), api_key.to_owned());
            Ok(())
        }
    }

    #[test]
    fn credential_store_contract_persists_keys_by_provider() {
        let store = MemoryCredentialStore::default();
        store.save("minimax", "secret-value").expect("save key");

        assert_eq!(
            store.load("minimax").expect("load key").as_deref(),
            Some("secret-value")
        );
        assert_eq!(store.load("anthropic").expect("missing key"), None);
    }
}
