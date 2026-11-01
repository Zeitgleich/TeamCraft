-- A token per conversation and per space that changes whenever its member list answer would
-- (issue #714): `GET /sync/heads` names it instead of digesting every member row of every
-- conversation of the caller on each request (300 channels of 200 members: 60,000 rows).
--
-- The answer shows each member's role (and, in a space, expiry and sponsor) and the member's
-- username, display name, kind, organization, deactivation and deletion; the triggers below renew
-- the token when any of these change. A random token rather than a counter: after a restore from a
-- backup a counter would hand out numbers clients already saw for other contents. No row means no
-- change since this migration.
CREATE TABLE conversation_member_tokens (
  conversation_id bigint PRIMARY KEY REFERENCES conversations (id) ON DELETE CASCADE,
  token           uuid   NOT NULL
);

CREATE TABLE space_member_tokens (
  space_id bigint PRIMARY KEY REFERENCES spaces (id) ON DELETE CASCADE,
  token    uuid   NOT NULL
);

-- Statement-level, so a bulk change (an import, a cascade) renews each token once. Conversations
-- and spaces deleted in the same statement are skipped; their tokens go with them. In ID order, so
-- concurrent renewals lock rows in the same order.
CREATE FUNCTION renew_conversation_member_tokens(ids bigint[]) RETURNS void
LANGUAGE sql AS $$
  INSERT INTO conversation_member_tokens (conversation_id, token)
  SELECT id, gen_random_uuid()
  FROM (SELECT DISTINCT unnest(ids) AS id) AS changed
  WHERE EXISTS (SELECT 1 FROM conversations c WHERE c.id = changed.id)
  ORDER BY id
  ON CONFLICT (conversation_id) DO UPDATE SET token = EXCLUDED.token
$$;

CREATE FUNCTION renew_space_member_tokens(ids bigint[]) RETURNS void
LANGUAGE sql AS $$
  INSERT INTO space_member_tokens (space_id, token)
  SELECT id, gen_random_uuid()
  FROM (SELECT DISTINCT unnest(ids) AS id) AS changed
  WHERE EXISTS (SELECT 1 FROM spaces s WHERE s.id = changed.id)
  ORDER BY id
  ON CONFLICT (space_id) DO UPDATE SET token = EXCLUDED.token
$$;

-- memberships: added, removed, role changed.
CREATE FUNCTION memberships_added_renew_tokens() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
  PERFORM renew_conversation_member_tokens(ARRAY(SELECT conversation_id FROM added));
  RETURN NULL;
END
$$;

CREATE FUNCTION memberships_removed_renew_tokens() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
  PERFORM renew_conversation_member_tokens(ARRAY(SELECT conversation_id FROM removed));
  RETURN NULL;
END
$$;

CREATE FUNCTION memberships_changed_renew_tokens() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
  PERFORM renew_conversation_member_tokens(ARRAY(
    SELECT n.conversation_id
    FROM before_change o
    JOIN after_change n ON n.conversation_id = o.conversation_id AND n.user_id = o.user_id
    WHERE n.role IS DISTINCT FROM o.role));
  RETURN NULL;
END
$$;

CREATE TRIGGER memberships_added_tokens AFTER INSERT ON memberships
  REFERENCING NEW TABLE AS added
  FOR EACH STATEMENT EXECUTE FUNCTION memberships_added_renew_tokens();
CREATE TRIGGER memberships_removed_tokens AFTER DELETE ON memberships
  REFERENCING OLD TABLE AS removed
  FOR EACH STATEMENT EXECUTE FUNCTION memberships_removed_renew_tokens();
CREATE TRIGGER memberships_changed_tokens AFTER UPDATE ON memberships
  REFERENCING OLD TABLE AS before_change NEW TABLE AS after_change
  FOR EACH STATEMENT EXECUTE FUNCTION memberships_changed_renew_tokens();

-- space_members: added, removed, role, expiry or sponsor changed.
CREATE FUNCTION space_members_added_renew_tokens() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
  PERFORM renew_space_member_tokens(ARRAY(SELECT space_id FROM added));
  RETURN NULL;
END
$$;

CREATE FUNCTION space_members_removed_renew_tokens() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
  PERFORM renew_space_member_tokens(ARRAY(SELECT space_id FROM removed));
  RETURN NULL;
END
$$;

CREATE FUNCTION space_members_changed_renew_tokens() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
  PERFORM renew_space_member_tokens(ARRAY(
    SELECT n.space_id
    FROM before_change o
    JOIN after_change n ON n.space_id = o.space_id AND n.user_id = o.user_id
    WHERE (n.role, n.expires_at, n.sponsor_user_id) IS DISTINCT FROM (o.role, o.expires_at, o.sponsor_user_id)));
  RETURN NULL;
END
$$;

CREATE TRIGGER space_members_added_tokens AFTER INSERT ON space_members
  REFERENCING NEW TABLE AS added
  FOR EACH STATEMENT EXECUTE FUNCTION space_members_added_renew_tokens();
CREATE TRIGGER space_members_removed_tokens AFTER DELETE ON space_members
  REFERENCING OLD TABLE AS removed
  FOR EACH STATEMENT EXECUTE FUNCTION space_members_removed_renew_tokens();
CREATE TRIGGER space_members_changed_tokens AFTER UPDATE ON space_members
  REFERENCING OLD TABLE AS before_change NEW TABLE AS after_change
  FOR EACH STATEMENT EXECUTE FUNCTION space_members_changed_renew_tokens();

-- users: what a member list shows of a person changed; every list naming them gets a new token.
CREATE FUNCTION users_changed_renew_tokens() RETURNS trigger
LANGUAGE plpgsql AS $$
DECLARE
  changed bigint[];
BEGIN
  changed := ARRAY(
    SELECT n.id
    FROM before_change o
    JOIN after_change n ON n.id = o.id
    WHERE (n.username, n.display_name, n.kind, n.organization, n.deactivated_at IS NULL, n.deleted_at IS NULL)
      IS DISTINCT FROM
          (o.username, o.display_name, o.kind, o.organization, o.deactivated_at IS NULL, o.deleted_at IS NULL));
  IF cardinality(changed) > 0 THEN
    PERFORM renew_conversation_member_tokens(ARRAY(
      SELECT conversation_id FROM memberships WHERE user_id = ANY (changed)));
    PERFORM renew_space_member_tokens(ARRAY(
      SELECT space_id FROM space_members WHERE user_id = ANY (changed)));
  END IF;
  RETURN NULL;
END
$$;

CREATE TRIGGER users_changed_tokens AFTER UPDATE ON users
  REFERENCING OLD TABLE AS before_change NEW TABLE AS after_change
  FOR EACH STATEMENT EXECUTE FUNCTION users_changed_renew_tokens();
