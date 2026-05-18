CREATE TABLE server_settings (
    id                  INTEGER PRIMARY KEY CHECK (id = 1),
    advertised_endpoint TEXT
) STRICT;

INSERT INTO server_settings (id, advertised_endpoint) VALUES (1, NULL);

ALTER TABLE client_enrollments ADD COLUMN advertised_endpoint TEXT;
