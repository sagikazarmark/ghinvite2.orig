#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RepositoryIdentity {
    full_name: String,
    separator: usize,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum RepositoryIdentityError {
    #[error("repository full name must be in owner/name form")]
    MissingSeparator,
    #[error("repository owner must not be empty")]
    EmptyOwner,
    #[error("repository name must not be empty")]
    EmptyName,
    #[error("repository full name must contain exactly one separator")]
    TooManySeparators,
}

impl RepositoryIdentity {
    pub fn parse(full_name: impl Into<String>) -> Result<Self, RepositoryIdentityError> {
        let full_name = full_name.into();
        let separator = full_name
            .find('/')
            .ok_or(RepositoryIdentityError::MissingSeparator)?;
        if separator == 0 {
            return Err(RepositoryIdentityError::EmptyOwner);
        }
        if separator + 1 == full_name.len() {
            return Err(RepositoryIdentityError::EmptyName);
        }
        if full_name[separator + 1..].contains('/') {
            return Err(RepositoryIdentityError::TooManySeparators);
        }
        Ok(Self {
            full_name,
            separator,
        })
    }

    pub fn owner(&self) -> &str {
        &self.full_name[..self.separator]
    }

    pub fn name(&self) -> &str {
        &self.full_name[self.separator + 1..]
    }

    pub fn full_name(&self) -> &str {
        &self.full_name
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_identity_exposes_owner_name_and_full_name() {
        let identity = RepositoryIdentity::parse("acme/api").unwrap();

        assert_eq!(identity.owner(), "acme");
        assert_eq!(identity.name(), "api");
        assert_eq!(identity.full_name(), "acme/api");
    }

    #[test]
    fn missing_slash_is_rejected() {
        assert_eq!(
            RepositoryIdentity::parse("acme-api").unwrap_err(),
            RepositoryIdentityError::MissingSeparator
        );
    }

    #[test]
    fn empty_owner_is_rejected() {
        assert_eq!(
            RepositoryIdentity::parse("/api").unwrap_err(),
            RepositoryIdentityError::EmptyOwner
        );
    }

    #[test]
    fn empty_repository_name_is_rejected() {
        assert_eq!(
            RepositoryIdentity::parse("acme/").unwrap_err(),
            RepositoryIdentityError::EmptyName
        );
    }

    #[test]
    fn multiple_slashes_are_rejected() {
        assert_eq!(
            RepositoryIdentity::parse("acme/team/api").unwrap_err(),
            RepositoryIdentityError::TooManySeparators
        );
    }

    #[test]
    fn display_full_name_is_preserved_exactly() {
        let identity = RepositoryIdentity::parse("Acme-Inc/api gateway").unwrap();

        assert_eq!(identity.owner(), "Acme-Inc");
        assert_eq!(identity.name(), "api gateway");
        assert_eq!(identity.full_name(), "Acme-Inc/api gateway");
    }
}
