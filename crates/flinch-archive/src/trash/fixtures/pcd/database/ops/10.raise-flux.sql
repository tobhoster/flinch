-- @operation: export
-- @entity: batch
-- @name: Raise FLUX in 1080p Compact (runs after op 2: ops order by number, not by name)

-- --- BEGIN op 10 ( update quality_profile "1080p Compact" )
update "quality_profile_custom_formats" set "score" = 150 where "quality_profile_name" = '1080p Compact' and "custom_format_name" = 'FLUX' and "score" = 100;
-- --- END op 10
