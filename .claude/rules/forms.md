# Forms

Every form and dialog that takes input — web, desktop, Android and iOS — handles errors the same
way. An error the user can't see is a silent failure (robustness.md, "User-visible status"). Plans
and reviews of a form check this list.

## Errors at the field

- **Each error sits at its field**, directly below it, in words that say what to do ("Pick at
  least one day."), through i18n. One alert at the bottom of the form is not enough: in a scrolling
  dialog it is out of sight.
- **Show every error at once**, in the order of the fields, not one per submit.
- **Mark the field:** `aria-invalid="true"` while it has an error, and `aria-describedby` naming the
  message's id (`crate::form_errors::error_id`). A group of controls (day chips, radio group) gets
  them on its container, with `role="group"` and `tabindex="-1"`, so it can take the focus.
- **On a refused submit**, move the focus to the first invalid field and scroll it to the middle of
  its scroller (`crate::form_errors::focus_first_invalid`; smooth only without reduced motion).
- **After a refused submit, errors follow the input**: they go away as the user fixes the field. Don't
  validate fields the user hasn't left or submitted yet.
- **Server refusals** of a field's value (taken name, invalid address) go to that field too; errors
  that belong to no field (network, permission) are a toast through `feedback::report_failure` or
  an alert next to the submit button.

## Inputs

- Every input has a visible `<label for>` (or an `aria-label` when the visible label is the row's
  text, e.g. "After [10] meetings").
- **Units are always visible** next to number inputs ("Every [2] weeks", "[30] min"), with plural
  forms through Fluent (`t_count`).
- **A placeholder is an example, never the meaning** of an empty field: an empty number field that
  means "no limit" is a separate choice (a radio "Never"), not a placeholder.
- Number inputs say their range (`min`, `max`, `inputmode="numeric"`), and the message names it.
- Choices that exclude each other are radios or a select; on/off is a checkbox or switch; several
  days are toggle chips with `aria-pressed`.
- Inputs that don't apply right now are disabled, not hidden, when hiding would move the layout.

## Submitting

- The submit button runs through `PendingAction`/`PendingButton`: disabled while it runs, a retry
  of a create request carries the same idempotency key.
- A dialog closes only once the server took the change; on failure it stays open with the input.
- Destructive submits (cancel, delete) confirm first and say what happens to whom ("Everyone
  invited is told").

## Mobile

- Android: `isError` and `supportingText` on the `TextField`, focus the first invalid field.
- iOS: the message below the field, `accessibilityValue` or `.accessibilityHint` naming the error,
  and `AccessibilityFocusState` on the first invalid field.
