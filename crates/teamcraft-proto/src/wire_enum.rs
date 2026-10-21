/// Defines a string-valued wire enum with an `Unknown(String)` catch-all.
///
/// The generated type serializes to its wire name, parses unknown names into `Unknown` and offers
/// `as_str()`. Existing wire names must never change (see code-style.md, "Types").
macro_rules! wire_enum {
  (
    $(#[$meta:meta])*
    $vis:vis enum $name:ident {
      $( $(#[$vmeta:meta])* $variant:ident => $wire:literal ),+ $(,)?
    }
  ) => {
    $(#[$meta])*
    #[derive(Debug, Clone, PartialEq, Eq, Hash, ::serde::Serialize, ::serde::Deserialize)]
    #[serde(from = "String", into = "String")]
    $vis enum $name {
      $( $(#[$vmeta])* $variant, )+
      /// A value this version does not know, kept verbatim.
      Unknown(String),
    }

    impl $name {
      /// The stable wire name.
      pub fn as_str(&self) -> &str {
        match self {
          $( Self::$variant => $wire, )+
          Self::Unknown(other) => other,
        }
      }
    }

    impl ::std::convert::From<String> for $name {
      fn from(value: String) -> Self {
        match value.as_str() {
          $( $wire => Self::$variant, )+
          _ => Self::Unknown(value),
        }
      }
    }

    impl ::std::convert::From<$name> for String {
      fn from(value: $name) -> Self {
        match value {
          $name::Unknown(other) => other,
          known => known.as_str().to_owned(),
        }
      }
    }

    impl ::std::fmt::Display for $name {
      fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
        f.write_str(self.as_str())
      }
    }

    impl ::utoipa::PartialSchema for $name {
      fn schema() -> ::utoipa::openapi::RefOr<::utoipa::openapi::schema::Schema> {
        ::utoipa::openapi::schema::ObjectBuilder::new()
          .schema_type(::utoipa::openapi::schema::SchemaType::Type(::utoipa::openapi::schema::Type::String))
          .enum_values(Some([ $( $wire ),+ ]))
          .description(Some("Unknown values must be tolerated by clients."))
          .into()
      }
    }

    impl ::utoipa::ToSchema for $name {}
  };
}

pub(crate) use wire_enum;
