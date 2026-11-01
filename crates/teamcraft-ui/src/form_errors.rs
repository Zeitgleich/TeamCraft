//! Errors of a form, as `.claude/rules/forms.md` wants them: each at its field (`aria-invalid`,
//! `aria-describedby` naming the message), and a refused submit moves the focus to the first invalid
//! field and brings it into view, so an error is never out of sight (issues #1006, #1054).
//!
//! A form keeps its errors in a [`FormErrors`], marks each field with [`FormErrors::invalid`] and
//! [`FormErrors::described`], renders [`FieldError`] directly below it, and on a refused submit
//! calls [`FormErrors::refuse`]. A server refusal that names a field's value (a taken name) goes to
//! that field through [`FormErrors::refuse_server`].

use leptos::prelude::*;
use teamcraft_proto::ErrorCode;
use wasm_bindgen::JsCast;

use crate::feedback::{ActionFailure, FailureKind};
use crate::motion::prefers_reduced_motion;

/// The errors of one form: reactive, `Copy`, owned by the form's component.
#[derive(Clone, Copy)]
pub struct FormErrors {
  errors: RwSignal<FieldErrors>,
  was_refused: RwSignal<bool>,
}

/// The errors of a form's fields, in the order of the fields. A field can have several; the first
/// one is shown.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FieldErrors {
  entries: Vec<FieldErrorEntry>,
}

/// One error: the field's element id and the localized message.
#[derive(Clone, Debug, PartialEq, Eq)]
struct FieldErrorEntry {
  field_id: String,
  message: String,
}

impl FormErrors {
  pub fn new() -> Self {
    Self {
      errors: RwSignal::new(FieldErrors::default()),
      was_refused: RwSignal::new(false),
    }
  }

  /// Shows `found` after a refused submit and moves the focus to the first invalid field inside
  /// the element `container_id`. From now on [`Self::follow`] keeps the errors in step with the
  /// input.
  pub fn refuse(&self, container_id: &'static str, found: FieldErrors) {
    self.errors.set(found);
    self.was_refused.set(true);
    focus_first_invalid(container_id);
  }

  /// Puts the server's refusal at its field when its code is one of `fields` (code → field id) and
  /// focuses that field. Returns false for a refusal that belongs to no field: it was shown as a
  /// toast already ([`crate::feedback`]). May run after the form is gone.
  pub fn refuse_server(
    &self,
    container_id: &'static str,
    failure: &ActionFailure,
    fields: &[(ErrorCode, &str)],
    message: String,
  ) -> bool {
    let Some(field_id) = server_error_field(failure, fields) else {
      return false;
    };
    let mut found = FieldErrors::default();
    found.push(field_id, message);
    if self.errors.try_set(found).is_some() {
      return false;
    }
    let _ = self.was_refused.try_set(true);
    focus_first_invalid(container_id);
    true
  }

  /// After a refused submit, replaces the errors with `found` as the user edits, so an error goes
  /// away once its field is fixed. Before the first refused submit nothing is shown: fields the
  /// user hasn't submitted yet aren't judged.
  pub fn follow(&self, found: FieldErrors) {
    if self.was_refused.get_untracked() {
      self.errors.set(found);
    }
  }

  /// Removes every error (the form was sent, or reset).
  pub fn clear(&self) {
    self.errors.set(FieldErrors::default());
    self.was_refused.set(false);
  }

  /// Removes the errors of one field, e.g. a server refusal once the user edits the value.
  pub fn clear_field(&self, field_id: &str) {
    if self.errors.with_untracked(|errors| errors.is_invalid(field_id)) {
      self.errors.update(|errors| errors.remove(field_id));
    }
  }

  /// Whether any field has an error (reactive).
  pub fn has_errors(&self) -> bool {
    self.errors.with(|errors| !errors.is_empty())
  }

  /// The field's `aria-invalid`: `"true"` while it has an error.
  pub fn invalid(self, field_id: impl Into<String>) -> impl Fn() -> Option<&'static str> + Clone + 'static {
    let errors = self.errors;
    let field_id = field_id.into();
    move || errors.with(|errors| errors.aria_invalid(&field_id))
  }

  /// The field's `aria-describedby`: its message while it has an error, otherwise `otherwise` (a
  /// hint the field points to anyway).
  pub fn described(self, field_id: impl Into<String>) -> impl Fn() -> Option<String> + Clone + 'static {
    self.described_or(field_id, None)
  }

  /// [`Self::described`] for a field that points to a hint while it has no error.
  pub fn described_or(
    self,
    field_id: impl Into<String>,
    maybe_hint_id: Option<&'static str>,
  ) -> impl Fn() -> Option<String> + Clone + 'static {
    let errors = self.errors;
    let field_id = field_id.into();
    move || {
      errors
        .with(|errors| errors.described_by(&field_id))
        .or_else(|| maybe_hint_id.map(str::to_owned))
    }
  }

  /// The message of the field's first error (reactive).
  pub fn message(&self, field_id: &str) -> Option<String> {
    self
      .errors
      .with(|errors| errors.message(field_id).map(str::to_owned))
  }
}

impl Default for FormErrors {
  fn default() -> Self {
    Self::new()
  }
}

impl FieldErrors {
  pub fn new() -> Self {
    Self::default()
  }

  /// Adds an error of `field_id`; fields are pushed in the order they appear in the form.
  pub fn push(&mut self, field_id: impl Into<String>, message: impl Into<String>) {
    self.entries.push(FieldErrorEntry {
      field_id: field_id.into(),
      message: message.into(),
    });
  }

  pub fn is_empty(&self) -> bool {
    self.entries.is_empty()
  }

  pub fn is_invalid(&self, field_id: &str) -> bool {
    self.entries.iter().any(|entry| entry.field_id == field_id)
  }

  /// The message shown below the field: its first error.
  pub fn message(&self, field_id: &str) -> Option<&str> {
    self
      .entries
      .iter()
      .find(|entry| entry.field_id == field_id)
      .map(|entry| entry.message.as_str())
  }

  /// The field the focus goes to on a refused submit.
  pub fn first_field(&self) -> Option<&str> {
    self.entries.first().map(|entry| entry.field_id.as_str())
  }

  fn aria_invalid(&self, field_id: &str) -> Option<&'static str> {
    self.is_invalid(field_id).then_some("true")
  }

  fn described_by(&self, field_id: &str) -> Option<String> {
    self.is_invalid(field_id).then(|| error_id(field_id))
  }

  fn remove(&mut self, field_id: &str) {
    self.entries.retain(|entry| entry.field_id != field_id);
  }
}

/// The message of a field, directly below it, where its `aria-describedby` points. Empty (and
/// hidden) while the field is fine.
#[component]
pub fn FieldError(errors: FormErrors, #[prop(into)] field: String) -> impl IntoView {
  let id = error_id(&field);
  view! { <p class="field-error" id=id>{move || errors.message(&field)}</p> }
}

/// The id of the message of the field `field_id`, for its `aria-describedby`.
pub fn error_id(field_id: &str) -> String {
  format!("{field_id}-error")
}

/// The field a server refusal belongs to: the field id paired with its error code in `fields`.
/// Refusals of other kinds (network, permission) belong to no field.
pub fn server_error_field<'a>(failure: &ActionFailure, fields: &[(ErrorCode, &'a str)]) -> Option<&'a str> {
  let FailureKind::Server(code) = &failure.kind else {
    return None;
  };
  fields
    .iter()
    .find(|(field_code, _)| field_code == code)
    .map(|(_, field_id)| *field_id)
}

/// After the errors are rendered (the next frame), focuses the first field marked
/// `aria-invalid="true"` inside the element `container_id` and scrolls it to the middle of the view.
pub fn focus_first_invalid(container_id: impl Into<String>) {
  let container_id = container_id.into();
  request_animation_frame(move || {
    let Some(field) = web_sys::window()
      .and_then(|window| window.document())
      .and_then(|document| document.get_element_by_id(&container_id))
      .and_then(|container| container.query_selector("[aria-invalid=\"true\"]").ok().flatten())
      .and_then(|field| field.dyn_into::<web_sys::HtmlElement>().ok())
    else {
      return;
    };
    let options = web_sys::ScrollIntoViewOptions::new();
    options.set_block(web_sys::ScrollLogicalPosition::Center);
    if !prefers_reduced_motion() {
      options.set_behavior(web_sys::ScrollBehavior::Smooth);
    }
    field.scroll_into_view_with_scroll_into_view_options(&options);
    // The scroll above places it; focusing must not jump again.
    let focus = web_sys::FocusOptions::new();
    focus.set_prevent_scroll(true);
    // Ignored on failure: an element that can't take the focus still shows its error.
    drop(field.focus_with_options(&focus));
  });
}

#[cfg(test)]
mod tests {
  use super::*;

  const NAME: &str = "group-name";
  const EMAIL: &str = "invite-email";

  mod field_errors {
    use super::*;

    #[test]
    fn a_field_without_an_error_is_neither_invalid_nor_described() {
      let errors = FieldErrors::new();
      assert!(errors.is_empty());
      assert_eq!(errors.aria_invalid(NAME), None);
      assert_eq!(errors.described_by(NAME), None);
      assert_eq!(errors.message(NAME), None);
      assert_eq!(errors.first_field(), None);
    }

    #[test]
    fn an_invalid_field_points_to_its_message() {
      let errors = refused();
      assert_eq!(errors.aria_invalid(NAME), Some("true"));
      assert_eq!(errors.described_by(NAME).as_deref(), Some("group-name-error"));
      assert_eq!(errors.aria_invalid("other"), None);
    }

    #[test]
    fn every_error_shows_and_the_first_field_takes_the_focus() {
      let errors = refused();
      assert_eq!(errors.first_field(), Some(NAME));
      assert_eq!(errors.message(NAME), Some("Enter a name."));
      assert_eq!(errors.message(EMAIL), Some("Enter an email address."));
    }

    #[test]
    fn a_field_shows_its_first_error() {
      let mut errors = refused();
      errors.push(NAME, "Too long.");
      assert_eq!(errors.message(NAME), Some("Enter a name."));
    }

    #[test]
    fn a_fixed_field_loses_its_error() {
      let mut errors = refused();
      errors.remove(NAME);
      assert!(!errors.is_invalid(NAME));
      assert_eq!(errors.first_field(), Some(EMAIL));
    }

    fn refused() -> FieldErrors {
      let mut errors = FieldErrors::new();
      errors.push(NAME, "Enter a name.");
      errors.push(EMAIL, "Enter an email address.");
      errors
    }
  }

  mod form_errors {
    use super::*;

    #[test]
    fn a_refused_submit_marks_and_describes_the_fields() {
      let owner = Owner::new();
      owner.with(|| {
        let form = FormErrors::new();
        let mut found = FieldErrors::new();
        found.push(NAME, "Enter a name.");
        form.errors.set(found);
        form.was_refused.set(true);
        assert_eq!(form.invalid(NAME)(), Some("true"));
        assert_eq!(form.described(NAME)().as_deref(), Some("group-name-error"));
        assert_eq!(
          form.described_or(EMAIL, Some("invite-hint"))().as_deref(),
          Some("invite-hint")
        );
      });
    }

    #[test]
    fn errors_follow_the_input_only_after_a_refused_submit() {
      let owner = Owner::new();
      owner.with(|| {
        let form = FormErrors::new();
        let mut found = FieldErrors::new();
        found.push(NAME, "Enter a name.");
        form.follow(found.clone());
        assert!(!form.has_errors());
        form.was_refused.set(true);
        form.follow(found);
        assert!(form.has_errors());
        form.follow(FieldErrors::new());
        assert!(!form.has_errors());
      });
    }

    #[test]
    fn editing_a_field_clears_its_server_refusal() {
      let owner = Owner::new();
      owner.with(|| {
        let form = FormErrors::new();
        let mut found = FieldErrors::new();
        found.push(NAME, "Taken.");
        found.push(EMAIL, "Enter an email address.");
        form.errors.set(found);
        form.clear_field(NAME);
        assert_eq!(form.invalid(NAME)(), None);
        assert_eq!(form.invalid(EMAIL)(), Some("true"));
      });
    }
  }

  mod server_errors {
    use super::*;

    #[test]
    fn a_refusal_of_the_value_lands_at_its_field() {
      let fields = [(ErrorCode::Conflict, NAME), (ErrorCode::InvalidRequest, EMAIL)];
      assert_eq!(
        server_error_field(&server(ErrorCode::Conflict), &fields),
        Some(NAME)
      );
      assert_eq!(
        server_error_field(&server(ErrorCode::InvalidRequest), &fields),
        Some(EMAIL)
      );
    }

    #[test]
    fn other_refusals_belong_to_no_field() {
      let fields = [(ErrorCode::Conflict, NAME)];
      assert_eq!(server_error_field(&server(ErrorCode::Forbidden), &fields), None);
      let offline = ActionFailure {
        kind: FailureKind::Network,
        detail: String::new(),
      };
      assert_eq!(server_error_field(&offline, &fields), None);
    }

    fn server(code: ErrorCode) -> ActionFailure {
      ActionFailure {
        kind: FailureKind::Server(code),
        detail: String::new(),
      }
    }
  }
}
