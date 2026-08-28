ALTER TABLE publish_action DROP COLUMN next_attempt_at;
ALTER TABLE run DROP COLUMN summary;
ALTER TABLE run DROP COLUMN verdict;
ALTER TABLE run DROP COLUMN omitted_files_json;
ALTER TABLE run DROP COLUMN truncated;
