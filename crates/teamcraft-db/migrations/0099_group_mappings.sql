-- Identity provider groups to spaces and public channels (issue #210, docs/provisioning.md §5): the
-- sync adds the provider's members of a group to the rule's spaces and channels, once per account
-- and revision of the rule, so somebody who leaves a mapped channel isn't added back by every run.
CREATE TABLE identity_provider_group_mappings (
  id          bigint      PRIMARY KEY,
  -- The provider's group path, e.g. `/engineering`.
  group_path  text        NOT NULL UNIQUE,
  space_ids   bigint[]    NOT NULL,
  channel_ids bigint[]    NOT NULL DEFAULT '{}',
  -- Raised when the spaces or channels change: the rule applies once more to everybody in the group.
  revision    bigint      NOT NULL DEFAULT 1,
  created_at  timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE identity_provider_group_mapping_applied (
  mapping_id bigint      NOT NULL REFERENCES identity_provider_group_mappings (id) ON DELETE CASCADE,
  user_id    bigint      NOT NULL REFERENCES users (id) ON DELETE CASCADE,
  revision   bigint      NOT NULL,
  applied_at timestamptz NOT NULL DEFAULT now(),
  PRIMARY KEY (mapping_id, user_id)
);
