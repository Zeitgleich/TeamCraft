-- A member may replace the name from the identity provider with their own (issue #408, instance
-- policy `directory.allow_display_name_override`).
--
-- `display_name` stays the name everybody sees, so no reader changes: it is the override when one
-- is set and the provider's name otherwise. `idp_display_name` is what the provider last said; the
-- sign-in sync writes it and never touches the override, and clearing the override restores
-- `display_name` from it. NULL (accounts that never came from a provider, and rows from before this
-- migration) means `display_name` is the provider's name.
ALTER TABLE users
  ADD COLUMN idp_display_name text,
  ADD COLUMN display_name_override text;
