-- @operation: export
-- @entity: batch
-- @name: Add FLUX to 1080p Compact

-- --- BEGIN op 2 ( create custom_format "FLUX" )
insert into "regular_expressions" ("name", "pattern", "description", "regex101_id") values ('FLUX', '(?<=^|[\s.-])FLUX\b', NULL, NULL);
insert into "custom_formats" ("name", "description") values ('FLUX', 'The FLUX release group.');
INSERT INTO custom_format_conditions (custom_format_name, name, type, arr_type, negate, required)
VALUES ('FLUX', 'FLUX', 'release_group', 'all', 0, 0);
INSERT INTO condition_patterns (custom_format_name, condition_name, regular_expression_name) VALUES ('FLUX', 'FLUX', 'FLUX');
INSERT INTO quality_profile_custom_formats (quality_profile_name, custom_format_name, arr_type, score) VALUES ('1080p Compact', 'FLUX', 'all', 100);
-- --- END op 2
