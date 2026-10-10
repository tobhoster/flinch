-- @operation: export
-- @entity: batch
-- @name: FLINCH fixture: one profile in the Dictionarry database's style

INSERT INTO tags (name) VALUES ('Release Group');

INSERT INTO regular_expressions (name, pattern, description) VALUES ('x265', '[xh][ .]?265|\bHEVC(\b|\d)', 'Matches x265 and HEVC.');
INSERT INTO regular_expression_tags (regular_expression_name, tag_name) VALUES ('x265', 'Release Group');

INSERT INTO custom_formats (name, description) VALUES ('1080p WEB-DL', 'A 1080p WEB-DL.');
INSERT INTO custom_formats (name, description, include_in_rename) VALUES ('x265', 'An x265 encode.', 1);
INSERT INTO custom_formats (name, description) VALUES ('Freeleech', 'A freeleech torrent.');
INSERT INTO custom_formats (name, description) VALUES ('Season Pack', 'A whole season.');

INSERT INTO custom_format_conditions (custom_format_name, name, type, arr_type, negate, required)
SELECT cf.name, 'WEB-DL', 'source', 'all', 0, 1 FROM custom_formats cf WHERE cf.name = '1080p WEB-DL';
INSERT INTO condition_sources (custom_format_name, condition_name, source) VALUES ('1080p WEB-DL', 'WEB-DL', 'web_dl');
INSERT INTO custom_format_conditions (custom_format_name, name, type, arr_type, negate, required)
SELECT cf.name, '1080p', 'resolution', 'all', 0, 1 FROM custom_formats cf WHERE cf.name = '1080p WEB-DL';
INSERT INTO condition_resolutions (custom_format_name, condition_name, resolution) VALUES ('1080p WEB-DL', '1080p', '1080p');

INSERT INTO custom_format_conditions (custom_format_name, name, type, arr_type, negate, required)
VALUES ('x265', 'x265', 'release_title', 'all', 0, 0);
INSERT INTO condition_patterns (custom_format_name, condition_name, regular_expression_name) VALUES ('x265', 'x265', 'x265');

INSERT INTO custom_format_conditions (custom_format_name, name, type, arr_type, negate, required)
VALUES ('Freeleech', 'Freeleech', 'indexer_flag', 'all', 0, 0);
INSERT INTO condition_indexer_flags (custom_format_name, condition_name, flag) VALUES ('Freeleech', 'Freeleech', 'freeleech');

INSERT INTO custom_format_conditions (custom_format_name, name, type, arr_type, negate, required)
VALUES ('Season Pack', 'Season Pack', 'release_type', 'sonarr', 0, 0);
INSERT INTO condition_release_types (custom_format_name, condition_name, release_type) VALUES ('Season Pack', 'Season Pack', 'season_pack');

INSERT INTO quality_profiles (name, description, upgrades_allowed, minimum_custom_format_score, upgrade_until_score, upgrade_score_increment)
VALUES ('1080p Compact', '1080p Compact targets low to medium quality x265 encodes.', 1, 20000, 888888, 1);

INSERT INTO quality_groups (quality_profile_name, name) SELECT qp.name, '1080p Compact' FROM quality_profiles qp WHERE qp.name = '1080p Compact';
INSERT INTO quality_groups (quality_profile_name, name) SELECT qp.name, '720p Quality' FROM quality_profiles qp WHERE qp.name = '1080p Compact';

INSERT INTO quality_group_members (quality_profile_name, quality_group_name, quality_name, position)
SELECT '1080p Compact', '1080p Compact', q.name, 0 FROM qualities q WHERE q.name = 'WEBDL-1080p';
INSERT INTO quality_group_members (quality_profile_name, quality_group_name, quality_name, position)
SELECT '1080p Compact', '1080p Compact', q.name, 1 FROM qualities q WHERE q.name = 'WEBRip-1080p';
INSERT INTO quality_group_members (quality_profile_name, quality_group_name, quality_name, position)
SELECT '1080p Compact', '1080p Compact', q.name, 2 FROM qualities q WHERE q.name = 'Bluray-1080p';
INSERT INTO quality_group_members (quality_profile_name, quality_group_name, quality_name, position)
SELECT '1080p Compact', '720p Quality', q.name, 0 FROM qualities q WHERE q.name = 'WEBDL-720p';
INSERT INTO quality_group_members (quality_profile_name, quality_group_name, quality_name, position)
SELECT '1080p Compact', '720p Quality', q.name, 1 FROM qualities q WHERE q.name = 'Bluray-720p';

INSERT INTO quality_profile_qualities (quality_profile_name, quality_group_name, position, upgrade_until)
SELECT qp.name, qg.name, 0, 1 FROM quality_profiles qp, quality_groups qg
WHERE qp.name = '1080p Compact' AND qg.quality_profile_name = qp.name AND qg.name = '1080p Compact';
INSERT INTO quality_profile_qualities (quality_profile_name, quality_group_name, position, upgrade_until)
SELECT qp.name, qg.name, 1, 0 FROM quality_profiles qp, quality_groups qg
WHERE qp.name = '1080p Compact' AND qg.quality_profile_name = qp.name AND qg.name = '720p Quality';
INSERT INTO quality_profile_qualities (quality_profile_name, quality_name, position, enabled, upgrade_until)
VALUES ('1080p Compact', 'Remux-1080p', 2, 0, 0);

INSERT INTO quality_profile_custom_formats (quality_profile_name, custom_format_name, arr_type, score)
SELECT qp.name, cf.name, 'all', 280000 FROM quality_profiles qp, custom_formats cf WHERE qp.name = '1080p Compact' AND cf.name = '1080p WEB-DL';
INSERT INTO quality_profile_custom_formats (quality_profile_name, custom_format_name, arr_type, score)
SELECT qp.name, cf.name, 'all', 50 FROM quality_profiles qp, custom_formats cf WHERE qp.name = '1080p Compact' AND cf.name = 'x265';
INSERT INTO quality_profile_custom_formats (quality_profile_name, custom_format_name, arr_type, score)
SELECT qp.name, cf.name, 'sonarr', 75 FROM quality_profiles qp, custom_formats cf WHERE qp.name = '1080p Compact' AND cf.name = 'x265';
INSERT INTO quality_profile_custom_formats (quality_profile_name, custom_format_name, arr_type, score)
SELECT qp.name, cf.name, 'all', 5 FROM quality_profiles qp, custom_formats cf WHERE qp.name = '1080p Compact' AND cf.name = 'Freeleech';
INSERT INTO quality_profile_custom_formats (quality_profile_name, custom_format_name, arr_type, score)
SELECT qp.name, cf.name, 'sonarr', 10 FROM quality_profiles qp, custom_formats cf WHERE qp.name = '1080p Compact' AND cf.name = 'Season Pack';

INSERT INTO quality_profile_languages (quality_profile_name, language_name, type)
SELECT qp.name, l.name, 'must' FROM quality_profiles qp, languages l WHERE qp.name = '1080p Compact' AND l.name = 'Original';

INSERT INTO radarr_quality_definitions (name, quality_name, min_size, max_size, preferred_size)
SELECT 'default', m.quality_name, 5, 100, 95 FROM quality_api_mappings m WHERE m.arr_type = 'radarr' AND m.api_name = 'WEBDL-1080p';
INSERT INTO radarr_quality_definitions (name, quality_name, min_size, max_size, preferred_size)
SELECT 'default', m.quality_name, 5, 200, 190 FROM quality_api_mappings m WHERE m.arr_type = 'radarr' AND m.api_name = 'Bluray-1080p';
INSERT INTO sonarr_quality_definitions (name, quality_name, min_size, max_size, preferred_size)
SELECT 'default', m.quality_name, 3, 80, 75 FROM quality_api_mappings m WHERE m.arr_type = 'sonarr' AND m.api_name = 'Bluray-1080p Remux';
