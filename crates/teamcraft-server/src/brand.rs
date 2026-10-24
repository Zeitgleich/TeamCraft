//! Brands (docs/branding.md): several host names, each with its own presentation.
//!
//! A registry is built from `[branding]`, the `[[brands]]` of the configuration file and the brands
//! admins made in the console (stored in the database), and replaced as a whole when an admin
//! changes one. A request gets its brand from the host name alone, matched exactly against the
//! brands' host names; nothing the client sends besides `Host` (and `X-Forwarded-Host` behind a
//! trusted proxy) takes part, and the received host is never copied into a response.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::net::{IpAddr, SocketAddr};
use std::path::Path;
use std::sync::{Arc, PoisonError, RwLock};

use axum::body::Bytes;
use axum::extract::{ConnectInfo, FromRequestParts, Request, State};
use axum::http::request::Parts;
use axum::http::{HeaderMap, HeaderValue, Uri};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use teamcraft_proto::{
  BrandAssetSlot, BrandColors, BrandLinks, BrandResponse, BrandSource, BrandSummary, brand_color_properties,
  is_brand_hex_color,
};
use time::OffsetDateTime;

use crate::config::{BrandConfig, BrandMailConfig, BrandingConfig, MailConfig, UnknownHostPolicy, WebConfig};
use crate::error::ApiError;
use crate::state::AppState;

/// Id of the brand used when no `[[brands]]` is configured.
const BUILTIN_BRAND_ID: &str = "teamcraft";
const BUILTIN_DISPLAY_NAME: &str = "TeamCraft";
const MAX_BRAND_ID_LEN: usize = 32;
const MAX_TEXT_CHARS: usize = 200;
/// Longest provider name on the sign-in button.
const MAX_SSO_LABEL_CHARS: usize = 64;
/// Most host names and e-mail domains of one brand.
const MAX_LIST_ENTRIES: usize = 50;
const ASSET_EXTENSIONS: &[(&str, &str)] = &[
  ("svg", "image/svg+xml"),
  ("png", "image/png"),
  ("jpg", "image/jpeg"),
  ("jpeg", "image/jpeg"),
  ("webp", "image/webp"),
  ("ico", "image/x-icon"),
];

/// The brands in force, replaced as a whole when an admin changes one.
pub struct BrandStore {
  inputs: BrandInputs,
  current: RwLock<Arc<BrandRegistry>>,
  /// The brands from the database, with their image bytes, as the registry in force was built from.
  database: RwLock<Vec<DatabaseBrand>>,
}

/// What every registry is built from besides the database brands.
#[derive(Debug, Clone, Default)]
pub struct BrandInputs {
  pub branding: BrandingConfig,
  pub config_brands: Vec<BrandConfig>,
  pub web: WebConfig,
  pub mail: MailConfig,
}

/// A brand made or taken over in the admin console, with its images.
#[derive(Debug, Clone)]
pub struct DatabaseBrand {
  /// The settings; image file names are unused (the images are in `assets`).
  pub config: BrandConfig,
  pub updated_at: OffsetDateTime,
  /// Image by slot name, with its object storage key.
  pub assets: BTreeMap<String, StoredAsset>,
}

/// An image of a database brand.
#[derive(Debug, Clone)]
pub struct StoredAsset {
  pub object_key: String,
  pub content_type: &'static str,
  pub bytes: Bytes,
}

/// The brands of this server and how a host name selects one.
#[derive(Debug)]
pub struct BrandRegistry {
  brands: Vec<Arc<Brand>>,
  by_host: HashMap<String, Arc<Brand>>,
  default: Arc<Brand>,
  unknown_host: UnknownHostPolicy,
  trusted_proxies: Vec<IpAddr>,
  /// Host names brands may use: `[branding].served_hostnames` and those of `[[brands]]`.
  served_hostnames: BTreeSet<String>,
}

/// One validated brand with everything precomputed that is served unchanged.
#[derive(Debug)]
pub struct Brand {
  pub id: String,
  pub source: BrandSource,
  pub is_default: bool,
  /// A database brand that replaces the configuration brand with the same id.
  pub is_overriding_config: bool,
  pub maybe_updated_at: Option<OffsetDateTime>,
  /// The settings it was built from (image file names aside).
  pub config: BrandConfig,
  pub display_name: String,
  /// Canonical host name: the first configured one, empty if the brand has none.
  pub canonical_host: String,
  pub hostnames: Vec<String>,
  pub email_domains: Vec<String>,
  pub mail: BrandMailConfig,
  /// The body of `GET /api/v1/brand`.
  pub response: BrandResponse,
  /// The body of `GET /api/v1/brand/theme.css`.
  pub theme_css: String,
  /// `Content-Security-Policy` of the web client on this brand's hosts.
  pub content_security_policy: HeaderValue,
  assets: HashMap<String, BrandAsset>,
}

/// An image file of a brand, read at startup.
#[derive(Debug, Clone)]
pub struct BrandAsset {
  pub bytes: Bytes,
  pub content_type: &'static str,
  pub etag: HeaderValue,
}

/// The brand of the current request, put into the request extensions by [`resolve_brand`].
#[derive(Debug, Clone)]
pub struct ResolvedBrand {
  pub brand: Arc<Brand>,
  /// Whether the request host is one the brand lists (false: the default brand stands in).
  pub is_known_host: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum BrandError {
  #[error("brands: {0}")]
  Invalid(String),
  #[error("brand {brand}: asset {name:?}: {reason}")]
  Asset {
    brand: String,
    name: String,
    reason: String,
  },
}

impl BrandStore {
  /// The configuration's brands only; database brands are added with [`Self::install`].
  pub fn new(inputs: BrandInputs) -> Result<Self, BrandError> {
    let registry = BrandRegistry::build_with(&inputs, &[])?;
    Ok(Self {
      inputs,
      current: RwLock::new(Arc::new(registry)),
      database: RwLock::new(Vec::new()),
    })
  }

  /// The registry in force.
  pub fn current(&self) -> Arc<BrandRegistry> {
    self
      .current
      .read()
      .unwrap_or_else(PoisonError::into_inner)
      .clone()
  }

  /// The database brands the registry in force was built from.
  pub fn database_brands(&self) -> Vec<DatabaseBrand> {
    self
      .database
      .read()
      .unwrap_or_else(PoisonError::into_inner)
      .clone()
  }

  pub fn inputs(&self) -> &BrandInputs {
    &self.inputs
  }

  /// Builds the registry these database brands would give, without putting it in force.
  pub fn check(&self, database: &[DatabaseBrand]) -> Result<BrandRegistry, BrandError> {
    BrandRegistry::build_with(&self.inputs, database)
  }

  /// Puts a checked registry and the database brands it was built from in force.
  pub fn install(&self, registry: BrandRegistry, database: Vec<DatabaseBrand>) {
    *self.database.write().unwrap_or_else(PoisonError::into_inner) = database;
    *self.current.write().unwrap_or_else(PoisonError::into_inner) = Arc::new(registry);
  }
}

impl BrandRegistry {
  /// Validates the configuration and reads the asset files.
  pub fn build(
    branding: &BrandingConfig,
    brands: &[BrandConfig],
    web: &WebConfig,
    mail: &MailConfig,
  ) -> Result<Self, BrandError> {
    let inputs = BrandInputs {
      branding: branding.clone(),
      config_brands: brands.to_vec(),
      web: web.clone(),
      mail: mail.clone(),
    };
    Self::build_with(&inputs, &[])
  }

  /// The configuration's brands, replaced or extended by the database brands. A database brand is
  /// the default if it says so; else the configuration's default; else (without configured brands)
  /// the built-in one.
  pub fn build_with(inputs: &BrandInputs, database: &[DatabaseBrand]) -> Result<Self, BrandError> {
    let BrandInputs {
      branding,
      config_brands,
      web,
      mail,
    } = inputs;
    let served_hostnames = served_hostnames(branding, config_brands)?;
    let stored_ids: HashSet<&str> = database.iter().map(|brand| brand.config.id.as_str()).collect();
    let config_ids: HashSet<&str> = config_brands.iter().map(|brand| brand.id.as_str()).collect();
    let has_stored_default = database.iter().any(|brand| brand.config.is_default);
    let mut built = Vec::new();
    for config in config_brands {
      if stored_ids.contains(config.id.as_str()) {
        continue;
      }
      let mut config = config.clone();
      config.is_default &= !has_stored_default;
      let assets = load_assets(&config, branding, &config.id)?;
      built.push(Brand::build(&config, assets, web, BrandOrigin::Config)?);
    }
    for stored in database {
      check_served(&stored.config, &served_hostnames)?;
      let assets = stored_assets(&stored.assets, &stored.config.id)?;
      let origin = BrandOrigin::Database {
        is_overriding_config: config_ids.contains(stored.config.id.as_str()),
        updated_at: stored.updated_at,
      };
      built.push(Brand::build(&stored.config, assets, web, origin)?);
    }
    if config_brands.is_empty() && !built.iter().any(|brand| brand.is_default) {
      built.push(builtin_brand(web, mail)?);
    }
    Self::assemble(
      built.into_iter().map(Arc::new).collect(),
      branding,
      served_hostnames,
    )
  }

  fn assemble(
    built: Vec<Arc<Brand>>,
    branding: &BrandingConfig,
    served_hostnames: BTreeSet<String>,
  ) -> Result<Self, BrandError> {
    let mut defaults = built.iter().filter(|brand| brand.is_default);
    let default = defaults
      .next()
      .map(Arc::clone)
      .ok_or_else(|| BrandError::Invalid("exactly one brand needs is_default = true".into()))?;
    if defaults.next().is_some() {
      return Err(BrandError::Invalid(
        "more than one brand has is_default = true".into(),
      ));
    }
    let mut by_host = HashMap::new();
    let mut ids = HashSet::new();
    for brand in &built {
      if !ids.insert(brand.id.clone()) {
        return Err(BrandError::Invalid(format!(
          "brand id {:?} is used twice",
          brand.id
        )));
      }
      for host in &brand.hostnames {
        if by_host.insert(host.clone(), Arc::clone(brand)).is_some() {
          return Err(BrandError::Invalid(format!(
            "host name {host:?} belongs to two brands"
          )));
        }
      }
    }
    Ok(Self {
      brands: built,
      default,
      // With no brand host name there is nothing to refuse: the built-in brand serves every host.
      unknown_host: if by_host.is_empty() {
        UnknownHostPolicy::Default
      } else {
        branding.unknown_host
      },
      by_host,
      trusted_proxies: branding
        .trusted_proxies
        .iter()
        .map(IpAddr::to_canonical)
        .collect(),
      served_hostnames,
    })
  }

  /// The brand with one built-in default and nothing else; for tests and tools.
  #[allow(clippy::expect_used)]
  pub fn builtin() -> Self {
    let web = WebConfig::default();
    Self::build(&BrandingConfig::default(), &[], &web, &MailConfig::default())
      // Invariant: the built-in brand is valid by construction (covered by a test).
      .expect("the built-in brand is valid")
  }

  /// Every brand, default first.
  pub fn brands(&self) -> Vec<&Arc<Brand>> {
    let mut brands: Vec<&Arc<Brand>> = self.brands.iter().collect();
    brands.sort_by_key(|brand| brand.id != self.default.id);
    brands
  }

  /// Host names brands may use, each with the brand it selects now.
  pub fn served_hostnames(&self) -> Vec<(String, Option<String>)> {
    self
      .served_hostnames
      .iter()
      .map(|host| (host.clone(), self.by_host.get(host).map(|brand| brand.id.clone())))
      .collect()
  }

  /// Which brand serves a request with this host, and whether the host is one the brand lists.
  pub fn resolve(&self, host: Option<&str>) -> ResolvedBrand {
    let known = host
      .and_then(normalize_host)
      .and_then(|host| self.by_host.get(&host).cloned());
    match known {
      Some(brand) => ResolvedBrand {
        brand,
        is_known_host: true,
      },
      None => ResolvedBrand {
        brand: Arc::clone(&self.default),
        is_known_host: false,
      },
    }
  }

  /// Every brand, default first, for the signed-in list.
  pub fn summaries(&self) -> Vec<BrandSummary> {
    self
      .brands()
      .into_iter()
      .map(|brand| BrandSummary {
        id: brand.id.clone(),
        display_name: brand.display_name.clone(),
        colors: brand.response.colors.clone(),
      })
      .collect()
  }

  pub fn default_brand(&self) -> &Arc<Brand> {
    &self.default
  }

  pub fn by_id(&self, id: &str) -> Option<&Arc<Brand>> {
    self.brands.iter().find(|brand| brand.id == id)
  }

  /// The brand that lists the domain of an e-mail address, if any.
  pub fn find_by_email(&self, email: &str) -> Option<&Arc<Brand>> {
    let domain = email
      .rsplit_once('@')
      .map(|(_, domain)| domain.to_ascii_lowercase())?;
    self
      .brands
      .iter()
      .find(|brand| brand.email_domains.contains(&domain))
  }

  /// The home brand of an e-mail address: the brand that lists its domain, else the default.
  pub fn for_email(&self, email: &str) -> &Arc<Brand> {
    self.find_by_email(email).unwrap_or(&self.default)
  }

  /// The brand mail to `email` is sent as: the one that lists the recipient's domain, else the
  /// brand of the request that caused the mail.
  pub fn for_recipient<'a>(&'a self, email: &str, request: &'a Arc<Brand>) -> &'a Arc<Brand> {
    self.find_by_email(email).unwrap_or(request)
  }

  /// Whether a stored brand id still exists; links of removed brands fall back to the default.
  pub fn is_known_id(&self, id: &str) -> bool {
    self.by_id(id).is_some()
  }

  pub fn unknown_host_policy(&self) -> UnknownHostPolicy {
    self.unknown_host
  }

  /// The host name a request names, taken from `Host` (or `X-Forwarded-Host` if trusted).
  pub fn request_host(&self, headers: &HeaderMap, uri: &Uri, maybe_peer: Option<IpAddr>) -> Option<String> {
    let is_trusted = maybe_peer.is_some_and(|peer| self.trusted_proxies.contains(&peer.to_canonical()));
    if is_trusted && let Some(forwarded) = headers.get("x-forwarded-host").and_then(|v| v.to_str().ok()) {
      // The last entry is the one the nearest proxy added.
      return forwarded.rsplit(',').next().map(|host| host.trim().to_owned());
    }
    headers
      .get(axum::http::header::HOST)
      .and_then(|v| v.to_str().ok())
      .map(str::to_owned)
      .or_else(|| uri.host().map(str::to_owned))
  }
}

impl Brand {
  fn build(
    config: &BrandConfig,
    assets: HashMap<String, BrandAsset>,
    web: &WebConfig,
    origin: BrandOrigin,
  ) -> Result<Self, BrandError> {
    let id = config.id.clone();
    let invalid = |message: String| BrandError::Invalid(format!("brand {id:?}: {message}"));
    if id.is_empty()
      || id.len() > MAX_BRAND_ID_LEN
      || !id
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    {
      return Err(BrandError::Invalid(format!(
        "brand id {id:?} must be 1 to {MAX_BRAND_ID_LEN} characters of a-z, 0-9 and -"
      )));
    }
    if config.display_name.trim().is_empty() || config.display_name.chars().count() > MAX_TEXT_CHARS {
      return Err(invalid("display_name is empty or too long".into()));
    }
    if config.hostnames.len() > MAX_LIST_ENTRIES || config.email_domains.len() > MAX_LIST_ENTRIES {
      return Err(invalid(format!(
        "at most {MAX_LIST_ENTRIES} host names and e-mail domains"
      )));
    }
    if let Some(name) = &config.maybe_organization_name
      && (name.trim().is_empty()
        || name.chars().count() > MAX_TEXT_CHARS
        || name.chars().any(char::is_control))
    {
      return Err(invalid("organization_name must be short single-line text".into()));
    }
    if let Some(label) = &config.maybe_sso_label
      && (label.trim().is_empty()
        || label.chars().count() > MAX_SSO_LABEL_CHARS
        || label.chars().any(char::is_control))
    {
      return Err(invalid("sso_label must be short single-line text".into()));
    }
    if !config.is_default && config.hostnames.is_empty() {
      return Err(invalid(
        "a brand that is not the default needs at least one host name".into(),
      ));
    }
    for host in &config.hostnames {
      if normalize_host(host).as_deref() != Some(host.as_str()) {
        return Err(invalid(format!(
          "host name {host:?} must be a lower-case DNS name without port, scheme or path"
        )));
      }
    }
    let email_domains = config
      .email_domains
      .iter()
      .map(|domain| domain.to_ascii_lowercase())
      .collect::<Vec<_>>();
    for domain in &email_domains {
      if normalize_host(domain).as_deref() != Some(domain.as_str()) {
        return Err(invalid(format!("email domain {domain:?} is not a DNS name")));
      }
    }
    for text in [
      &config.mail.maybe_sender_name,
      &config.mail.maybe_imip_organizer_name,
    ]
    .into_iter()
    .flatten()
    {
      if text.trim().is_empty() || text.chars().count() > MAX_TEXT_CHARS || text.chars().any(char::is_control)
      {
        return Err(invalid("mail names must be short single-line text".into()));
      }
    }
    if let Some(address) = &config.mail.maybe_from_address
      && !address
        .parse::<lettre::Address>()
        .is_ok_and(|a| a.to_string() == *address)
    {
      return Err(invalid("mail.from_address is not an e-mail address".into()));
    }
    let links = BrandLinks {
      maybe_support: checked_url(config.maybe_support_url.as_deref(), "support_url", &id)?,
      maybe_privacy: checked_url(config.maybe_privacy_url.as_deref(), "privacy_url", &id)?,
      maybe_terms: checked_url(config.maybe_terms_url.as_deref(), "terms_url", &id)?,
      maybe_imprint: checked_url(config.maybe_imprint_url.as_deref(), "imprint_url", &id)?,
    };
    for texts in [&config.login_title, &config.login_text] {
      for (language, text) in texts {
        if language.is_empty()
          || language.len() > 16
          || !language.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
          || text.chars().count() > MAX_TEXT_CHARS * 5
        {
          return Err(invalid(format!(
            "login text for language {language:?} is invalid"
          )));
        }
      }
    }
    let colors = BrandColors {
      light: checked_colors(&config.colors.light, &id)?,
      dark: checked_colors(&config.colors.dark, &id)?,
    };
    let canonical_host = config.hostnames.first().cloned().unwrap_or_default();
    let response = BrandResponse {
      id: id.clone(),
      display_name: config.display_name.clone(),
      maybe_organization_name: config.maybe_organization_name.clone(),
      origin: origin_of(&canonical_host),
      assets: assets
        .keys()
        .map(|slot| (slot.clone(), format!("/api/v1/brand/assets/{slot}")))
        .collect(),
      colors: colors.clone(),
      login_title: config.login_title.clone(),
      login_text: config.login_text.clone(),
      links,
    };
    let (source, is_overriding_config, maybe_updated_at) = match origin {
      BrandOrigin::Builtin => (BrandSource::Builtin, false, None),
      BrandOrigin::Config => (BrandSource::Config, false, None),
      BrandOrigin::Database {
        is_overriding_config,
        updated_at,
      } => (BrandSource::Database, is_overriding_config, Some(updated_at)),
    };
    Ok(Self {
      content_security_policy: brand_policy(config, web, &id)?,
      theme_css: theme_css(&colors),
      source,
      is_default: config.is_default,
      is_overriding_config,
      maybe_updated_at,
      config: config.clone(),
      id,
      display_name: config.display_name.clone(),
      canonical_host,
      hostnames: config.hostnames.clone(),
      email_domains,
      mail: config.mail.clone(),
      response,
      assets,
    })
  }

  /// The image of a slot, if the brand has one.
  pub fn asset(&self, slot: &BrandAssetSlot) -> Option<&BrandAsset> {
    self.assets.get(slot.as_str())
  }

  /// The slots that have an image.
  pub fn asset_slots(&self) -> Vec<BrandAssetSlot> {
    let mut slots: Vec<BrandAssetSlot> = self
      .assets
      .keys()
      .map(|slot| BrandAssetSlot::from(slot.clone()))
      .collect();
    slots.sort_by(|a, b| a.as_str().cmp(b.as_str()));
    slots
  }

  /// The base URL links in mail are built on: the brand's own origin, else `fallback`.
  pub fn base_url(&self, fallback: &str) -> String {
    let origin = self.origin();
    if origin.is_empty() {
      fallback.trim_end_matches('/').to_owned()
    } else {
      origin
    }
  }

  /// Subject and body of the invitation e-mail: the brand's texts, else the global ones.
  pub fn invite_texts(&self, mail: &MailConfig) -> (String, String) {
    (
      self
        .mail
        .maybe_invite_subject
        .clone()
        .unwrap_or_else(|| mail.invite_subject.clone()),
      self
        .mail
        .maybe_invite_body
        .clone()
        .unwrap_or_else(|| mail.invite_body.clone()),
    )
  }

  /// Subject and body of the activation e-mail of local accounts.
  pub fn activation_texts(&self, mail: &MailConfig) -> (String, String) {
    (
      self
        .mail
        .maybe_activation_subject
        .clone()
        .unwrap_or_else(|| mail.activation_subject.clone()),
      self
        .mail
        .maybe_activation_body
        .clone()
        .unwrap_or_else(|| mail.activation_body.clone()),
    )
  }

  /// `https://<canonical host>`, empty if the brand has no host name. Links in mail, invitations
  /// and API bodies are built from this and never from the received host.
  pub fn origin(&self) -> String {
    origin_of(&self.canonical_host)
  }
}

impl<S: Send + Sync> FromRequestParts<S> for ResolvedBrand {
  type Rejection = ApiError;

  async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
    parts
      .extensions
      .get::<Self>()
      .cloned()
      .ok_or_else(|| ApiError::Internal("the brand middleware did not run".into()))
  }
}

/// Middleware: selects the brand of the request and refuses unknown hosts when so configured.
pub async fn resolve_brand(State(state): State<AppState>, mut request: Request, next: Next) -> Response {
  let resolved = {
    let registry = state.brands.current();
    let maybe_peer = request
      .extensions()
      .get::<ConnectInfo<SocketAddr>>()
      .map(|ConnectInfo(peer)| peer.ip());
    let host = registry.request_host(request.headers(), request.uri(), maybe_peer);
    let resolved = registry.resolve(host.as_deref());
    if !resolved.is_known_host
      && registry.unknown_host_policy() == UnknownHostPolicy::Reject
      && !is_host_independent(request.uri().path())
    {
      return ApiError::UnknownHost.into_response();
    }
    resolved
  };
  request.extensions_mut().insert(resolved);
  next.run(request).await
}

/// Paths that are reached by IP, internal names or other servers and so cannot be required to
/// carry a brand host.
fn is_host_independent(path: &str) -> bool {
  path == "/healthz"
    || path == "/readyz"
    || path == "/api/v1/auth/backchannel-logout"
    || path.starts_with("/api/internal/")
}

/// The content type of an uploaded brand image, judged by its first bytes; `None` for anything that
/// is not PNG, JPEG, WebP, ICO or SVG, or does not match the type the uploader declared.
pub fn sniff_image(bytes: &[u8], declared: &str) -> Option<&'static str> {
  let declared = declared
    .split(';')
    .next()
    .unwrap_or_default()
    .trim()
    .to_ascii_lowercase();
  let sniffed = if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
    "image/png"
  } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
    "image/jpeg"
  } else if bytes.len() > 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
    "image/webp"
  } else if bytes.starts_with(&[0, 0, 1, 0]) {
    "image/x-icon"
  } else if is_svg(bytes) {
    "image/svg+xml"
  } else {
    return None;
  };
  let is_declared = declared == sniffed
    || (sniffed == "image/x-icon" && declared == "image/vnd.microsoft.icon")
    || (sniffed == "image/jpeg" && declared == "image/jpg");
  is_declared.then_some(sniffed)
}

/// UTF-8 text whose first element is `<svg` (after an optional XML declaration, comments, a doctype
/// and white space). It is only ever shown through `<img>` and served under a locked-down policy.
fn is_svg(bytes: &[u8]) -> bool {
  let Ok(text) = std::str::from_utf8(bytes) else {
    return false;
  };
  let mut rest = text.trim_start_matches('\u{feff}').trim_start();
  loop {
    if let Some(after) = rest.strip_prefix("<?") {
      let Some((_, tail)) = after.split_once("?>") else {
        return false;
      };
      rest = tail.trim_start();
    } else if let Some(after) = rest.strip_prefix("<!--") {
      let Some((_, tail)) = after.split_once("-->") else {
        return false;
      };
      rest = tail.trim_start();
    } else if let Some(after) = rest.strip_prefix("<!DOCTYPE") {
      let Some((_, tail)) = after.split_once('>') else {
        return false;
      };
      rest = tail.trim_start();
    } else {
      return rest.starts_with("<svg") && rest.contains("</svg>")
        || rest.starts_with("<svg") && rest.contains("/>");
    }
  }
}

/// Lower-cases a host name, drops a port and a trailing dot, and accepts DNS names only: no IP
/// literals, no userinfo, no empty or over-long labels. `None` for anything else.
pub fn normalize_host(raw: &str) -> Option<String> {
  let raw = raw.trim();
  if raw.is_empty() || raw.starts_with('[') {
    return None;
  }
  let without_port = match raw.rsplit_once(':') {
    Some((host, port)) if !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()) => host,
    Some(_) => return None,
    None => raw,
  };
  let host = without_port
    .strip_suffix('.')
    .unwrap_or(without_port)
    .to_ascii_lowercase();
  if host.is_empty() || host.len() > 253 || host.parse::<IpAddr>().is_ok() {
    return None;
  }
  let is_valid = host.split('.').all(|label| {
    !label.is_empty()
      && label.len() <= 63
      && !label.starts_with('-')
      && !label.ends_with('-')
      && label
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
  });
  // `1.2.3` style names are numeric addresses to some resolvers; a brand needs a real name.
  let is_numeric = host
    .rsplit('.')
    .next()
    .is_some_and(|tld| tld.bytes().all(|b| b.is_ascii_digit()));
  (is_valid && !is_numeric).then_some(host)
}

fn builtin_brand(web: &WebConfig, mail: &MailConfig) -> Result<Brand, BrandError> {
  let config = BrandConfig {
    id: BUILTIN_BRAND_ID.into(),
    is_default: true,
    display_name: BUILTIN_DISPLAY_NAME.into(),
    ..BrandConfig::default()
  };
  let mut brand = Brand::build(&config, HashMap::new(), web, BrandOrigin::Builtin)?;
  // Without brands the base URL of the mail settings is the only address the server knows.
  brand.response.origin = mail.public_base_url.trim_end_matches('/').to_owned();
  Ok(brand)
}

/// Where a brand being built comes from.
enum BrandOrigin {
  Builtin,
  Config,
  Database {
    is_overriding_config: bool,
    updated_at: OffsetDateTime,
  },
}

/// `[branding].served_hostnames` and the host names of `[[brands]]`: the names the proxy serves.
fn served_hostnames(
  branding: &BrandingConfig,
  config_brands: &[BrandConfig],
) -> Result<BTreeSet<String>, BrandError> {
  let mut served = BTreeSet::new();
  for host in &branding.served_hostnames {
    if normalize_host(host).as_deref() != Some(host.as_str()) {
      return Err(BrandError::Invalid(format!(
        "branding.served_hostnames: {host:?} must be a lower-case DNS name without port, scheme or path"
      )));
    }
    served.insert(host.clone());
  }
  served.extend(
    config_brands
      .iter()
      .flat_map(|brand| brand.hostnames.iter().cloned()),
  );
  Ok(served)
}

/// A database brand may only use host names the server is deployed for.
fn check_served(config: &BrandConfig, served: &BTreeSet<String>) -> Result<(), BrandError> {
  match config.hostnames.iter().find(|host| !served.contains(*host)) {
    Some(host) => Err(BrandError::Invalid(format!(
      "brand {:?}: host name {host:?} is not served by this server (add it to [branding].served_hostnames and the proxy)",
      config.id
    ))),
    None => Ok(()),
  }
}

/// The images of a database brand, ready to serve.
fn stored_assets(
  stored: &BTreeMap<String, StoredAsset>,
  brand: &str,
) -> Result<HashMap<String, BrandAsset>, BrandError> {
  stored
    .iter()
    .map(|(slot, asset)| {
      let etag = asset_etag(&asset.bytes).ok_or_else(|| BrandError::Asset {
        brand: brand.to_owned(),
        name: slot.clone(),
        reason: "cannot compute a validator".into(),
      })?;
      Ok((
        slot.clone(),
        BrandAsset {
          bytes: asset.bytes.clone(),
          content_type: asset.content_type,
          etag,
        },
      ))
    })
    .collect()
}

fn asset_etag(bytes: &[u8]) -> Option<HeaderValue> {
  HeaderValue::from_str(&format!(
    "\"{}\"",
    teamcraft_crypto::account::subresource_integrity(bytes)
  ))
  .ok()
}

fn origin_of(host: &str) -> String {
  if host.is_empty() {
    String::new()
  } else if host == "localhost" || host.ends_with(".localhost") {
    format!("http://{host}")
  } else {
    format!("https://{host}")
  }
}

fn checked_url(maybe_url: Option<&str>, key: &str, brand: &str) -> Result<Option<String>, BrandError> {
  let Some(raw) = maybe_url else { return Ok(None) };
  let invalid = || BrandError::Invalid(format!("brand {brand:?}: {key} must be an https URL"));
  let url = url::Url::parse(raw).map_err(|_| invalid())?;
  let is_local = url
    .host_str()
    .is_some_and(|host| host == "localhost" || host.ends_with(".localhost"));
  let is_scheme_ok = url.scheme() == "https" || (url.scheme() == "http" && is_local);
  if !is_scheme_ok || url.host_str().is_none() || !url.username().is_empty() || url.password().is_some() {
    return Err(invalid());
  }
  Ok(Some(url.into()))
}

fn checked_colors(
  colors: &BTreeMap<String, String>,
  brand: &str,
) -> Result<BTreeMap<String, String>, BrandError> {
  for (key, value) in colors {
    if brand_color_properties(key).is_none() {
      return Err(BrandError::Invalid(format!(
        "brand {brand:?}: {key:?} is not a brandable color"
      )));
    }
    if !is_brand_hex_color(value) {
      return Err(BrandError::Invalid(format!(
        "brand {brand:?}: color {key} must be #rgb or #rrggbb"
      )));
    }
  }
  Ok(colors.clone())
}

/// The brand's CSP: its own policy or the global one, with the brand's extra connect origins.
fn brand_policy(config: &BrandConfig, web: &WebConfig, brand: &str) -> Result<HeaderValue, BrandError> {
  let mut effective = web.clone();
  if let Some(policy) = &config.maybe_content_security_policy {
    effective.content_security_policy.clone_from(policy);
  }
  for origin in &config.connect_origins {
    if !effective.connect_origins.contains(origin) {
      effective.connect_origins.push(origin.clone());
    }
  }
  effective
    .validate()
    .map_err(|message| BrandError::Invalid(format!("brand {brand:?}: {message}")))?;
  HeaderValue::from_str(&effective.resolved_content_security_policy())
    .map_err(|_| BrandError::Invalid(format!("brand {brand:?}: the content security policy is invalid")))
}

/// The colors as CSS custom properties, in the three places `main.css` sets its own: the base, the
/// dark preference unless light is forced, and forced dark. Only validated hex values and fixed
/// property names are written.
fn theme_css(colors: &BrandColors) -> String {
  let declarations = |values: &BTreeMap<String, String>| {
    let mut out = String::new();
    for (key, value) in values {
      for property in brand_color_properties(key).unwrap_or_default() {
        out.push_str(&format!("{property}:{value};"));
      }
    }
    out
  };
  let light = declarations(&colors.light);
  let dark = declarations(&colors.dark);
  let mut css = String::new();
  if !light.is_empty() {
    css.push_str(&format!(":root{{{light}}}\n"));
  }
  if !dark.is_empty() {
    css.push_str(&format!(
      "@media (prefers-color-scheme: dark){{:root:not([data-theme=\"light\"]){{{dark}}}}}\n:root[data-theme=\"dark\"]{{{dark}}}\n"
    ));
  }
  css
}

fn load_assets(
  config: &BrandConfig,
  branding: &BrandingConfig,
  brand: &str,
) -> Result<HashMap<String, BrandAsset>, BrandError> {
  let slots = [
    (BrandAssetSlot::Logo, &config.maybe_logo),
    (BrandAssetSlot::LogoDark, &config.maybe_logo_dark),
    (BrandAssetSlot::Icon, &config.maybe_icon),
    (BrandAssetSlot::IconDark, &config.maybe_icon_dark),
    (BrandAssetSlot::Favicon, &config.maybe_favicon),
    (BrandAssetSlot::FaviconDark, &config.maybe_favicon_dark),
    (BrandAssetSlot::SsoIcon, &config.maybe_sso_icon),
  ];
  let mut assets = HashMap::new();
  for (slot, maybe_name) in slots {
    let Some(name) = maybe_name else { continue };
    let fail = |reason: String| BrandError::Asset {
      brand: brand.to_owned(),
      name: name.clone(),
      reason,
    };
    let dir = branding
      .maybe_assets_dir
      .as_deref()
      .ok_or_else(|| fail("[branding].assets_dir is not set".into()))?;
    let (bytes, content_type) = read_asset(dir, name, branding.max_asset_bytes).map_err(fail)?;
    let etag = asset_etag(&bytes).ok_or_else(|| fail("cannot compute a validator".into()))?;
    assets.insert(
      slot.as_str().to_owned(),
      BrandAsset {
        bytes: Bytes::from(bytes),
        content_type,
        etag,
      },
    );
  }
  Ok(assets)
}

/// Reads a regular file that lies inside `dir` (symbolic links may not lead out of it).
fn read_asset(dir: &Path, name: &str, max_bytes: u64) -> Result<(Vec<u8>, &'static str), String> {
  let relative = Path::new(name);
  let is_plain = !name.is_empty()
    && relative
      .components()
      .all(|part| matches!(part, std::path::Component::Normal(_)));
  if !is_plain {
    return Err("must be a relative path inside assets_dir".into());
  }
  let extension = relative
    .extension()
    .and_then(|e| e.to_str())
    .map(str::to_ascii_lowercase)
    .unwrap_or_default();
  let content_type = ASSET_EXTENSIONS
    .iter()
    .find(|(known, _)| *known == extension)
    .map(|(_, content_type)| *content_type)
    .ok_or_else(|| "must be svg, png, jpg, webp or ico".to_owned())?;
  let root = dir.canonicalize().map_err(|e| format!("assets_dir: {e}"))?;
  let path = root.join(relative).canonicalize().map_err(|e| e.to_string())?;
  if !path.starts_with(&root) {
    return Err("lies outside assets_dir".into());
  }
  let metadata = std::fs::metadata(&path).map_err(|e| e.to_string())?;
  if !metadata.is_file() {
    return Err("is not a regular file".into());
  }
  if metadata.len() > max_bytes {
    return Err(format!("is larger than {max_bytes} bytes"));
  }
  let bytes = std::fs::read(&path).map_err(|e| e.to_string())?;
  Ok((bytes, content_type))
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::config::BrandColorsConfig;

  fn brand(id: &str, hosts: &[&str], is_default: bool) -> BrandConfig {
    BrandConfig {
      id: id.into(),
      is_default,
      hostnames: hosts.iter().map(|h| (*h).to_owned()).collect(),
      display_name: format!("Brand {id}"),
      ..BrandConfig::default()
    }
  }

  fn registry(brands: &[BrandConfig], unknown: UnknownHostPolicy) -> Result<BrandRegistry, BrandError> {
    let branding = BrandingConfig {
      unknown_host: unknown,
      ..BrandingConfig::default()
    };
    BrandRegistry::build(&branding, brands, &WebConfig::default(), &MailConfig::default())
  }

  fn two_brands() -> Vec<BrandConfig> {
    vec![
      brand("a", &["chat.a.example.com", "chat.a.example.org"], true),
      brand("b", &["chat.b.example.com"], false),
    ]
  }

  mod host_selection {
    use super::*;

    #[test]
    fn exact_hosts_select_their_brand_ignoring_case_port_and_trailing_dot() {
      let registry = registry(&two_brands(), UnknownHostPolicy::Default).unwrap();
      for (host, id) in [
        ("chat.a.example.com", "a"),
        ("CHAT.B.example.com", "b"),
        ("chat.b.example.com:8443", "b"),
        ("chat.a.example.org.", "a"),
      ] {
        let resolved = registry.resolve(Some(host));
        assert!(resolved.is_known_host, "{host}");
        assert_eq!(resolved.brand.id, id, "{host}");
      }
    }

    #[test]
    fn look_alike_and_malformed_hosts_get_the_default_and_are_not_known() {
      let registry = registry(&two_brands(), UnknownHostPolicy::Default).unwrap();
      for host in [
        "evil-chat.b.example.com",
        "chat.b.example.com.evil.example",
        "xchat.b.example.com",
        "chat.b.example.com:notaport",
        "user@chat.b.example.com",
        "chat.b.example.com/path",
        "[::1]",
        "127.0.0.1",
        "",
      ] {
        let resolved = registry.resolve(Some(host));
        assert!(!resolved.is_known_host, "{host}");
        assert_eq!(resolved.brand.id, "a", "{host}");
      }
      assert!(!registry.resolve(None).is_known_host);
    }

    #[test]
    fn the_forwarded_host_counts_only_when_trusted() {
      let mut headers = HeaderMap::new();
      headers.insert("host", HeaderValue::from_static("chat.a.example.com"));
      headers.insert(
        "x-forwarded-host",
        HeaderValue::from_static("forged.example, chat.b.example.com"),
      );
      let uri: Uri = "/".parse().unwrap();
      let untrusted = registry(&two_brands(), UnknownHostPolicy::Default).unwrap();
      assert_eq!(
        untrusted
          .request_host(&headers, &uri, Some("192.0.2.9".parse().unwrap()))
          .as_deref(),
        Some("chat.a.example.com")
      );
      let branding = BrandingConfig {
        trusted_proxies: vec!["192.0.2.1".parse().unwrap()],
        ..BrandingConfig::default()
      };
      let trusted = BrandRegistry::build(
        &branding,
        &two_brands(),
        &WebConfig::default(),
        &MailConfig::default(),
      )
      .unwrap();
      assert_eq!(
        trusted
          .request_host(&headers, &uri, Some("192.0.2.1".parse().unwrap()))
          .as_deref(),
        Some("chat.b.example.com")
      );
      // The same proxy address as an IPv4-mapped IPv6 peer (dual-stack listener) counts too.
      assert_eq!(
        trusted
          .request_host(&headers, &uri, Some("::ffff:192.0.2.1".parse().unwrap()))
          .as_deref(),
        Some("chat.b.example.com")
      );
      // Another peer, or none, gets its own Host.
      for peer in [Some("192.0.2.2".parse().unwrap()), None] {
        assert_eq!(
          trusted.request_host(&headers, &uri, peer).as_deref(),
          Some("chat.a.example.com")
        );
      }
    }

    #[test]
    fn home_brand_follows_the_email_domain() {
      let mut brands = two_brands();
      brands[1].email_domains = vec!["B.example.com".into()];
      let registry = registry(&brands, UnknownHostPolicy::Default).unwrap();
      assert_eq!(registry.for_email("max@b.EXAMPLE.com").id, "b");
      assert_eq!(registry.for_email("max@other.example.com").id, "a");
      assert_eq!(registry.for_email("no-at-sign").id, "a");
    }
  }

  mod validation {
    use super::*;

    #[test]
    fn exactly_one_default_and_unique_ids_and_hosts() {
      let mut none_default = two_brands();
      none_default[0].is_default = false;
      assert!(registry(&none_default, UnknownHostPolicy::Default).is_err());
      let mut two_defaults = two_brands();
      two_defaults[1].is_default = true;
      assert!(registry(&two_defaults, UnknownHostPolicy::Default).is_err());
      let mut same_id = two_brands();
      same_id[1].id = "a".into();
      assert!(registry(&same_id, UnknownHostPolicy::Default).is_err());
      let mut same_host = two_brands();
      same_host[1].hostnames = vec!["chat.a.example.com".into()];
      assert!(registry(&same_host, UnknownHostPolicy::Default).is_err());
    }

    #[test]
    fn hosts_must_be_normalized_dns_names() {
      for bad in [
        "Chat.A.example.com",
        "chat.a.example.com:443",
        "https://chat.a.example.com",
        "10.0.0.1",
        "a b",
      ] {
        let brands = vec![brand("a", &[bad], true)];
        assert!(registry(&brands, UnknownHostPolicy::Default).is_err(), "{bad}");
      }
      let brands = vec![brand("a", &["localhost"], true)];
      assert!(registry(&brands, UnknownHostPolicy::Default).is_ok());
    }

    #[test]
    fn colors_must_be_plain_hex_for_known_tokens() {
      for (key, value) in [
        ("accent", "red"),
        ("accent", "#12"),
        ("accent", "#fff;}x{"),
        ("danger", "#fff"),
      ] {
        let mut brands = two_brands();
        brands[0].colors = BrandColorsConfig {
          light: BTreeMap::from([(key.to_owned(), value.to_owned())]),
          dark: BTreeMap::new(),
        };
        assert!(
          registry(&brands, UnknownHostPolicy::Default).is_err(),
          "{key} {value}"
        );
      }
    }

    #[test]
    fn links_must_be_https_without_credentials() {
      for bad in [
        "http://support.example.com",
        "javascript:alert(1)",
        "https://u:p@support.example.com",
        "nonsense",
      ] {
        let mut brands = two_brands();
        brands[0].maybe_support_url = Some(bad.into());
        assert!(registry(&brands, UnknownHostPolicy::Default).is_err(), "{bad}");
      }
      let mut brands = two_brands();
      brands[0].maybe_support_url = Some("https://support.example.com/help".into());
      assert!(registry(&brands, UnknownHostPolicy::Default).is_ok());
    }

    #[test]
    fn unknown_hosts_are_rejected_by_default_but_only_when_there_are_brand_hosts() {
      assert_eq!(BrandingConfig::default().unknown_host, UnknownHostPolicy::Reject);
      let with_hosts = BrandRegistry::build(
        &BrandingConfig::default(),
        &two_brands(),
        &WebConfig::default(),
        &MailConfig::default(),
      )
      .unwrap();
      assert_eq!(with_hosts.unknown_host_policy(), UnknownHostPolicy::Reject);
      // Nothing to match against: the built-in brand serves every host.
      assert_eq!(
        BrandRegistry::builtin().unknown_host_policy(),
        UnknownHostPolicy::Default
      );
    }

    #[test]
    fn the_builtin_brand_is_valid() {
      let registry = BrandRegistry::builtin();
      assert_eq!(registry.default_brand().id, "teamcraft");
      assert!(!registry.resolve(Some("anything.example.com")).is_known_host);
    }
  }

  mod assets {
    use super::*;

    fn registry_with_asset(name: &str, dir: &Path) -> Result<BrandRegistry, BrandError> {
      let mut brands = two_brands();
      brands[0].maybe_logo = Some(name.into());
      let branding = BrandingConfig {
        maybe_assets_dir: Some(dir.to_owned()),
        max_asset_bytes: 1024,
        ..BrandingConfig::default()
      };
      BrandRegistry::build(&branding, &brands, &WebConfig::default(), &MailConfig::default())
    }

    #[test]
    fn a_file_inside_the_directory_is_served_and_anything_else_is_refused() {
      let dir = tempfile_dir("assets");
      std::fs::create_dir_all(dir.join("a")).unwrap();
      std::fs::write(
        dir.join("a/logo.svg"),
        b"<svg xmlns=\"http://www.w3.org/2000/svg\"/>",
      )
      .unwrap();
      std::fs::write(dir.join("a/big.png"), vec![0u8; 2048]).unwrap();
      std::fs::write(dir.join("a/page.html"), b"<html>").unwrap();
      let outside = dir.parent().unwrap().join("outside.svg");
      std::fs::write(&outside, b"<svg/>").unwrap();

      let registry = registry_with_asset("a/logo.svg", &dir).unwrap();
      let logo = registry.default_brand().asset(&BrandAssetSlot::Logo).unwrap();
      assert_eq!(logo.content_type, "image/svg+xml");
      assert_eq!(
        registry
          .default_brand()
          .response
          .assets
          .get("logo")
          .map(String::as_str),
        Some("/api/v1/brand/assets/logo")
      );
      assert!(registry.default_brand().asset(&BrandAssetSlot::Icon).is_none());

      for bad in [
        "a/big.png",
        "a/page.html",
        "../outside.svg",
        "/etc/hostname",
        "a/missing.svg",
        "a",
      ] {
        assert!(registry_with_asset(bad, &dir).is_err(), "{bad}");
      }
      std::fs::remove_dir_all(&dir).unwrap();
      std::fs::remove_file(outside).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn a_symbolic_link_out_of_the_directory_is_refused() {
      let dir = tempfile_dir("links");
      let outside = dir.parent().unwrap().join("secret.png");
      std::fs::write(&outside, b"png").unwrap();
      std::os::unix::fs::symlink(&outside, dir.join("link.png")).unwrap();
      assert!(registry_with_asset("link.png", &dir).is_err());
      std::fs::remove_dir_all(&dir).unwrap();
      std::fs::remove_file(outside).unwrap();
    }

    #[test]
    fn assets_need_a_directory() {
      let mut brands = two_brands();
      brands[0].maybe_logo = Some("logo.svg".into());
      assert!(registry(&brands, UnknownHostPolicy::Default).is_err());
    }
  }

  mod theme_and_policy {
    use super::*;

    #[test]
    fn the_theme_contains_only_known_properties_and_hex_values() {
      let mut brands = two_brands();
      brands[0].colors = BrandColorsConfig {
        light: BTreeMap::from([("accent".to_owned(), "#0b7f78".to_owned())]),
        dark: BTreeMap::from([("accent".to_owned(), "#35c6ba".to_owned())]),
      };
      let registry = registry(&brands, UnknownHostPolicy::Default).unwrap();
      let css = &registry.default_brand().theme_css;
      assert!(css.contains(":root{--accent:#0b7f78;--focus:#0b7f78;}"), "{css}");
      assert!(
        css.contains(
          "@media (prefers-color-scheme: dark){:root:not([data-theme=\"light\"]){--accent:#35c6ba;"
        ),
        "{css}"
      );
      assert!(
        css.contains(":root[data-theme=\"dark\"]{--accent:#35c6ba;"),
        "{css}"
      );
      assert!(registry.by_id("b").unwrap().theme_css.is_empty());
    }

    #[test]
    fn each_brand_gets_its_own_policy() {
      let mut brands = two_brands();
      brands[1].connect_origins = vec!["https://api.b.example.com".into()];
      let registry = registry(&brands, UnknownHostPolicy::Default).unwrap();
      let policy = |id: &str| {
        registry
          .by_id(id)
          .unwrap()
          .content_security_policy
          .to_str()
          .unwrap()
          .to_owned()
      };
      assert!(
        policy("b").contains("connect-src 'self' https://api.b.example.com;"),
        "{}",
        policy("b")
      );
      assert!(!policy("a").contains("api.b.example.com"));
      brands[0].maybe_content_security_policy = Some("default-src 'none'".into());
      let registry = self::registry(&brands, UnknownHostPolicy::Default).unwrap();
      assert_eq!(
        registry
          .by_id("a")
          .unwrap()
          .content_security_policy
          .to_str()
          .unwrap(),
        "default-src 'none'"
      );
      brands[0].maybe_content_security_policy = Some("default-src 'self'\nscript-src *".into());
      assert!(self::registry(&brands, UnknownHostPolicy::Default).is_err());
    }

    #[test]
    fn origins_come_from_configured_hosts_only() {
      let registry = registry(&two_brands(), UnknownHostPolicy::Default).unwrap();
      assert_eq!(registry.default_brand().origin(), "https://chat.a.example.com");
      assert_eq!(
        registry.default_brand().response.origin,
        "https://chat.a.example.com"
      );
      let local = self::registry(&[brand("l", &["localhost"], true)], UnknownHostPolicy::Default).unwrap();
      assert_eq!(local.default_brand().origin(), "http://localhost");
    }
  }

  mod database_brands {
    use super::*;

    fn inputs(served: &[&str]) -> BrandInputs {
      BrandInputs {
        branding: BrandingConfig {
          served_hostnames: served.iter().map(|h| (*h).to_owned()).collect(),
          ..BrandingConfig::default()
        },
        config_brands: two_brands(),
        web: WebConfig::default(),
        mail: MailConfig::default(),
      }
    }

    fn stored(config: BrandConfig) -> DatabaseBrand {
      DatabaseBrand {
        config,
        updated_at: OffsetDateTime::UNIX_EPOCH,
        assets: BTreeMap::new(),
      }
    }

    #[test]
    fn a_stored_brand_replaces_the_configured_one_with_its_id() {
      let mut replacement = brand("b", &["chat.b.example.com"], false);
      replacement.display_name = "Brand B, edited".into();
      let registry = BrandRegistry::build_with(&inputs(&[]), &[stored(replacement)]).unwrap();
      let b = registry.by_id("b").unwrap();
      assert_eq!(b.display_name, "Brand B, edited");
      assert_eq!(b.source, BrandSource::Database);
      assert!(b.is_overriding_config);
      assert_eq!(registry.by_id("a").unwrap().source, BrandSource::Config);
    }

    #[test]
    fn only_served_host_names_and_one_default() {
      let unserved = stored(brand("c", &["chat.c.example.com"], false));
      assert!(BrandRegistry::build_with(&inputs(&[]), std::slice::from_ref(&unserved)).is_err());
      let registry = BrandRegistry::build_with(&inputs(&["chat.c.example.com"]), &[unserved]).unwrap();
      assert_eq!(registry.resolve(Some("chat.c.example.com")).brand.id, "c");
      assert!(
        registry
          .served_hostnames()
          .contains(&("chat.c.example.com".to_owned(), Some("c".to_owned())))
      );
      // A stored default takes the role from the configuration's.
      let default = stored(brand("c", &["chat.c.example.com"], true));
      let registry = BrandRegistry::build_with(&inputs(&["chat.c.example.com"]), &[default]).unwrap();
      assert_eq!(registry.default_brand().id, "c");
      assert!(!registry.by_id("a").unwrap().is_default);
      // A host name of a configured brand cannot be taken.
      let thief = stored(brand("c", &["chat.a.example.com"], false));
      assert!(BrandRegistry::build_with(&inputs(&[]), &[thief]).is_err());
    }

    #[test]
    fn without_configured_brands_the_builtin_stays_default_until_replaced() {
      let inputs = BrandInputs {
        branding: BrandingConfig {
          served_hostnames: vec!["chat.c.example.com".into()],
          ..BrandingConfig::default()
        },
        ..BrandInputs::default()
      };
      let extra = stored(brand("c", &["chat.c.example.com"], false));
      let registry = BrandRegistry::build_with(&inputs, &[extra]).unwrap();
      assert_eq!(registry.default_brand().source, BrandSource::Builtin);
      let default = stored(brand("c", &["chat.c.example.com"], true));
      let registry = BrandRegistry::build_with(&inputs, &[default]).unwrap();
      assert!(registry.by_id(BUILTIN_BRAND_ID).is_none());
    }

    #[test]
    fn uploaded_images_are_sniffed_and_must_match_the_declared_type() {
      assert_eq!(
        sniff_image(b"\x89PNG\r\n\x1a\nrest", "image/png"),
        Some("image/png")
      );
      assert_eq!(
        sniff_image(&[0xff, 0xd8, 0xff, 0xe0], "image/jpg"),
        Some("image/jpeg")
      );
      assert_eq!(
        sniff_image(b"RIFF\0\0\0\0WEBPVP8 ", "image/webp"),
        Some("image/webp")
      );
      assert_eq!(
        sniff_image(&[0, 0, 1, 0, 1], "image/vnd.microsoft.icon"),
        Some("image/x-icon")
      );
      let svg = b"<?xml version=\"1.0\"?>\n<!-- logo --><svg xmlns=\"http://www.w3.org/2000/svg\"></svg>";
      assert_eq!(
        sniff_image(svg, "image/svg+xml; charset=utf-8"),
        Some("image/svg+xml")
      );
      assert_eq!(sniff_image(b"\x89PNG\r\n\x1a\n", "image/jpeg"), None);
      assert_eq!(sniff_image(b"<html><svg/></html>", "image/svg+xml"), None);
      assert_eq!(sniff_image(b"", "image/png"), None);
    }
  }

  mod config_file {
    use super::*;
    use crate::config::Config;

    #[test]
    fn the_documented_toml_shape_parses_and_builds() {
      let config = Config::from_toml(
        r##"
        [database]
        url = "postgres://u:p@localhost/db"
        [oidc]
        issuer = "https://auth.example.com/realms/company"
        [branding]
        unknown_host = "reject"
        [[brands]]
        id = "brand-a"
        is_default = true
        hostnames = ["chat.a.example.com"]
        display_name = "Brand A"
        support_url = "https://support.a.example.com"
        [brands.login_title]
        en = "Welcome"
        de = "Willkommen"
        [brands.colors.light]
        accent = "#0b7f78"
        [brands.mail]
        sender_name = "Brand A Team"
        [[brands]]
        id = "brand-b"
        hostnames = ["chat.b.example.com"]
        display_name = "Brand B"
        "##,
      )
      .unwrap();
      let registry =
        BrandRegistry::build(&config.branding, &config.brands, &config.web, &config.mail).unwrap();
      assert_eq!(registry.unknown_host_policy(), UnknownHostPolicy::Reject);
      let a = registry.resolve(Some("chat.a.example.com")).brand;
      assert_eq!(a.response.login_title["de"], "Willkommen");
      assert_eq!(a.mail.maybe_sender_name.as_deref(), Some("Brand A Team"));
      assert_eq!(registry.resolve(Some("chat.b.example.com")).brand.id, "brand-b");
    }

    #[test]
    fn unknown_keys_are_refused() {
      let toml = r#"
        [database]
        url = "postgres://u:p@localhost/db"
        [oidc]
        issuer = "https://auth.example.com/realms/company"
        [[brands]]
        id = "a"
        colour = "red"
      "#;
      assert!(Config::from_toml(toml).is_err());
    }
  }

  fn tempfile_dir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("teamcraft-brand-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
  }
}
