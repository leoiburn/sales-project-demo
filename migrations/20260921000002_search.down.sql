-- Reverses 20260921000002_search.up.sql.
drop function if exists search_inventory(uuid, text, text, text, smallint, bigint, integer, text, text, text, int);
drop function if exists match_documents(uuid, vector, int, float);
