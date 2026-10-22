//! Brands (docs/branding.md): several host names, each with its own presentation.
//!
//! The registry is built once at startup from `[branding]` and `[[brands]]` and is read-only
//! afterwards. A request gets its brand from the host name alone, matched exactly against the
//! configured list; nothing the client sends besides `Host` (and `X-Forwarded-Host` behind a trusted
//! proxy) takes part, and the received host is never copied into a response.

use std::collections::{BTreeMap, HashMap};
use std::net::IpAddr;
use std::path::Path;
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{FromRequestParts, Request, State};
use axum::http::request::Parts;
use axum::http::{HeaderMap, HeaderValue, Uri};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use teamcraft_proto::{
  BrandAssetSlot, BrandColors, BrandLinks, BrandResponse, brand_color_properties, is_brand_hex_color,
};

use crate::config::{
  BrandConfig, BrandMailConfig, BrandingConfig, MailConfig, UnknownHostPolicy, WebConfig,
};
use crate::error::ApiError;
use crate::state::AppState;

/// Id of the brand used when no `[[brands]]` is configured.
const BUILTIN_BRAND_ID: &str = "teamcraft";
const BUILTIN_DISPLAY_NAME: &str = "TeamCraft";
const MAX_BRAND_ID_LEN: usize = 32;
const MAX_TEXT_CHARS: usize = 200;
const ASSET_EXTENSIONS: &[(&str, &str)] = &[
  ("svg", "image/svg+xml"),
  ("png", "image/png"),
  ("jpg", "image/jpeg"),
  ("jpeg", "image/jpeg"),
  ("webp", "image/webp"),
  ("ico", "image/x-icon"),
];

/// The brands of this server and how a host name selects one.
#[derive(Debug)]
pub struct BrandRegistry {
  brands: Vec<Arc<Brand>>,
  by_host: HashMap<String, Arc<Brand>>,
  default: Arc<Brand>,
  unknown_host: UnknownHostPolicy,
  trust_forwarded_host: bool,
}

/// One validated brand with everything precomputed that is served unchanged.
#[derive(Debug)]
pub struct Brand {
  pub id: String,
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

impl BrandRegistry {
  /// Validates the configuration and reads the asset files.
  pub fn build(
    branding: &BrandingConfig,
    brands: &[BrandConfig],
    web: &WebConfig,
    mail: &MailConfig,
  ) -> Result<Self, BrandError> {
    let built = if brands.is_empty() {
      vec![Arc::new(builtin_brand(web, mail)?)]
    } else {
      brands
        .iter()
        .map(|config| Brand::build(config, branding, web).map(Arc::new))
        .collect::<Result<Vec<_>, _>>()?
    };
    let mut defaults = built
      .iter()
      .zip(brands_or_builtin(brands))
      .filter(|(_, is_default)| *is_default);
    let default = defaults
      .next()
      .map(|(brand, _)| Arc::clone(brand))
      .ok_or_else(|| BrandError::Invalid("exactly one brand needs is_default = true".into()))?;
    if defaults.next().is_some() {
      return Err(BrandError::Invalid("more than one brand has is_default = true".into()));
    }
    let mut by_host = HashMap::new();
    let mut ids = std::collections::HashSet::new();
    for brand in &built {
      if !ids.insert(brand.id.clone()) {
        return Err(BrandError::Invalid(format!("brand id {:?} is used twice", brand.id)));
      }
      for host in &brand.hostnames {
        if by_host.insert(host.clone(), Arc::clone(brand)).is_some() {
          return Err(BrandError::Invalid(format!("host name {host:?} belongs to two brands")));
        }
      }
    }
    Ok(Self {
      brands: built,
      by_host,
      default,
      unknown_host: branding.unknown_host,
      trust_forwarded_host: branding.trust_forwarded_host,
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

  pub fn default_brand(&self) -> &Arc<Brand> {
    &self.default
  }

  pub fn by_id(&self, id: &str) -> Option<&Arc<Brand>> {
    self.brands.iter().find(|brand| brand.id == id)
  }

  /// The home brand of an e-mail address: the brand that lists its domain, else the default.
  pub fn for_email(&self, email: &str) -> &Arc<Brand> {
    let domain = email.rsplit_once('@').map(|(_, domain)| domain.to_ascii_lowercase());
    domain
      .and_then(|domain| {
        self
          .brands
          .iter()
          .find(|brand| brand.email_domains.iter().any(|d| *d == domain))
      })
      .unwrap_or(&self.default)
  }

  /// Whether a stored brand id still exists; links of removed brands fall back to the default.
  pub fn is_known_id(&self, id: &str) -> bool {
    self.by_id(id).is_some()
  }

  pub fn unknown_host_policy(&self) -> UnknownHostPolicy {
    self.unknown_host
  }

  /// The host name a request names, taken from `Host` (or `X-Forwarded-Host` if trusted).
  pub fn request_host(&self, headers: &HeaderMap, uri: &Uri) -> Option<String> {
    if self.trust_forwarded_host
      && let Some(forwarded) = headers.get("x-forwarded-host").and_then(|v| v.to_str().ok())
    {
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
  fn build(config: &BrandConfig, branding: &BrandingConfig, web: &WebConfig) -> Result<Self, BrandError> {
    let id = config.id.clone();
    let invalid = |message: String| BrandError::Invalid(format!("brand {id:?}: {message}"));
    if id.is_empty()
      || id.len() > MAX_BRAND_ID_LEN
      || !id.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    {
      return Err(BrandError::Invalid(format!(
        "brand id {id:?} must be 1 to {MAX_BRAND_ID_LEN} characters of a-z, 0-9 and -"
      )));
    }
    if config.display_name.trim().is_empty() || config.display_name.chars().count() > MAX_TEXT_CHARS {
      return Err(invalid("display_name is empty or too long".into()));
    }
    if !config.is_default && config.hostnames.is_empty() {
      return Err(invalid("a brand that is not the default needs at least one host name".into()));
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
          return Err(invalid(format!("login text for language {language:?} is invalid")));
        }
      }
    }
    let colors = BrandColors {
      light: checked_colors(&config.colors.light, &id)?,
      dark: checked_colors(&config.colors.dark, &id)?,
    };
    let assets = load_assets(config, branding, &id)?;
    let canonical_host = config.hostnames.first().cloned().unwrap_or_default();
    let response = BrandResponse {
      id: id.clone(),
      display_name: config.display_name.clone(),
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
    Ok(Self {
      content_security_policy: brand_policy(config, web, &id)?,
      theme_css: theme_css(&colors),
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
  let registry = &state.brands;
  let host = registry.request_host(request.headers(), request.uri());
  let resolved = registry.resolve(host.as_deref());
  if !resolved.is_known_host
    && registry.unknown_host_policy() == UnknownHostPolicy::Reject
    && !is_host_independent(request.uri().path())
  {
    return ApiError::UnknownHost.into_response();
  }
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
  let host = without_port.strip_suffix('.').unwrap_or(without_port).to_ascii_lowercase();
  if host.is_empty() || host.len() > 253 || host.parse::<IpAddr>().is_ok() {
    return None;
  }
  let is_valid = host.split('.').all(|label| {
    !label.is_empty()
      && label.len() <= 63
      && !label.starts_with('-')
      && !label.ends_with('-')
      && label.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
  });
  // `1.2.3` style names are numeric addresses to some resolvers; a brand needs a real name.
  let is_numeric = host.rsplit('.').next().is_some_and(|tld| tld.bytes().all(|b| b.is_ascii_digit()));
  (is_valid && !is_numeric).then_some(host)
}

fn builtin_brand(web: &WebConfig, mail: &MailConfig) -> Result<Brand, BrandError> {
  let config = BrandConfig {
    id: BUILTIN_BRAND_ID.into(),
    is_default: true,
    display_name: BUILTIN_DISPLAY_NAME.into(),
    ..BrandConfig::default()
  };
  let mut brand = Brand::build(&config, &BrandingConfig::default(), web)?;
  // Without brands the base URL of the mail settings is the only address the server knows.
  brand.response.origin = mail.public_base_url.trim_end_matches('/').to_owned();
  Ok(brand)
}

fn brands_or_builtin(brands: &[BrandConfig]) -> Vec<bool> {
  if brands.is_empty() {
    vec![true]
  } else {
    brands.iter().map(|brand| brand.is_default).collect()
  }
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
      return Err(BrandError::Invalid(format!("brand {brand:?}: {key:?} is not a brandable color")));
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
    let etag = HeaderValue::from_str(&format!("\"{}\"", teamcraft_crypto::account::subresource_integrity(&bytes)))
      .map_err(|_| fail("cannot compute a validator".into()))?;
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
      headers.insert("x-forwarded-host", HeaderValue::from_static("forged.example, chat.b.example.com"));
      let uri: Uri = "/".parse().unwrap();
      let untrusted = registry(&two_brands(), UnknownHostPolicy::Default).unwrap();
      assert_eq!(untrusted.request_host(&headers, &uri).as_deref(), Some("chat.a.example.com"));
      let branding = BrandingConfig {
        trust_forwarded_host: true,
        ..BrandingConfig::default()
      };
      let trusted =
        BrandRegistry::build(&branding, &two_brands(), &WebConfig::default(), &MailConfig::default()).unwrap();
      assert_eq!(trusted.request_host(&headers, &uri).as_deref(), Some("chat.b.example.com"));
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
      for bad in ["Chat.A.example.com", "chat.a.example.com:443", "https://chat.a.example.com", "10.0.0.1", "a b"] {
        let brands = vec![brand("a", &[bad], true)];
        assert!(registry(&brands, UnknownHostPolicy::Default).is_err(), "{bad}");
      }
      let brands = vec![brand("a", &["localhost"], true)];
      assert!(registry(&brands, UnknownHostPolicy::Default).is_ok());
    }

    #[test]
    fn colors_must_be_plain_hex_for_known_tokens() {
      for (key, value) in [("accent", "red"), ("accent", "#12"), ("accent", "#fff;}x{"), ("danger", "#fff")] {
        let mut brands = two_brands();
        brands[0].colors = BrandColorsConfig {
          light: BTreeMap::from([(key.to_owned(), value.to_owned())]),
          dark: BTreeMap::new(),
        };
        assert!(registry(&brands, UnknownHostPolicy::Default).is_err(), "{key} {value}");
      }
    }

    #[test]
    fn links_must_be_https_without_credentials() {
      for bad in ["http://support.example.com", "javascript:alert(1)", "https://u:p@support.example.com", "nonsense"] {
        let mut brands = two_brands();
        brands[0].maybe_support_url = Some(bad.into());
        assert!(registry(&brands, UnknownHostPolicy::Default).is_err(), "{bad}");
      }
      let mut brands = two_brands();
      brands[0].maybe_support_url = Some("https://support.example.com/help".into());
      assert!(registry(&brands, UnknownHostPolicy::Default).is_ok());
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
      std::fs::write(dir.join("a/logo.svg"), b"<svg xmlns=\"http://www.w3.org/2000/svg\"/>").unwrap();
      std::fs::write(dir.join("a/big.png"), vec![0u8; 2048]).unwrap();
      std::fs::write(dir.join("a/page.html"), b"<html>").unwrap();
      let outside = dir.parent().unwrap().join("outside.svg");
      std::fs::write(&outside, b"<svg/>").unwrap();

      let registry = registry_with_asset("a/logo.svg", &dir).unwrap();
      let logo = registry.default_brand().asset(&BrandAssetSlot::Logo).unwrap();
      assert_eq!(logo.content_type, "image/svg+xml");
      assert_eq!(
        registry.default_brand().response.assets.get("logo").map(String::as_str),
        Some("/api/v1/brand/assets/logo")
      );
      assert!(registry.default_brand().asset(&BrandAssetSlot::Icon).is_none());

      for bad in ["a/big.png", "a/page.html", "../outside.svg", "/etc/hostname", "a/missing.svg", "a"] {
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
      assert!(css.contains("@media (prefers-color-scheme: dark){:root:not([data-theme=\"light\"]){--accent:#35c6ba;"), "{css}");
      assert!(css.contains(":root[data-theme=\"dark\"]{--accent:#35c6ba;"), "{css}");
      assert!(registry.by_id("b").unwrap().theme_css.is_empty());
    }

    #[test]
    fn each_brand_gets_its_own_policy() {
      let mut brands = two_brands();
      brands[1].connect_origins = vec!["https://api.b.example.com".into()];
      let registry = registry(&brands, UnknownHostPolicy::Default).unwrap();
      let policy = |id: &str| registry.by_id(id).unwrap().content_security_policy.to_str().unwrap().to_owned();
      assert!(policy("b").contains("connect-src 'self' https://api.b.example.com;"), "{}", policy("b"));
      assert!(!policy("a").contains("api.b.example.com"));
      brands[0].maybe_content_security_policy = Some("default-src 'none'".into());
      let registry = self::registry(&brands, UnknownHostPolicy::Default).unwrap();
      assert_eq!(
        registry.by_id("a").unwrap().content_security_policy.to_str().unwrap(),
        "default-src 'none'"
      );
      brands[0].maybe_content_security_policy = Some("default-src 'self'\nscript-src *".into());
      assert!(self::registry(&brands, UnknownHostPolicy::Default).is_err());
    }

    #[test]
    fn origins_come_from_configured_hosts_only() {
      let registry = registry(&two_brands(), UnknownHostPolicy::Default).unwrap();
      assert_eq!(registry.default_brand().origin(), "https://chat.a.example.com");
      assert_eq!(registry.default_brand().response.origin, "https://chat.a.example.com");
      let local = self::registry(&[brand("l", &["localhost"], true)], UnknownHostPolicy::Default).unwrap();
      assert_eq!(local.default_brand().origin(), "http://localhost");
    }
  }

  fn tempfile_dir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("teamcraft-brand-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
  }
}
