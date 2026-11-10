-- Every continuation mutation invalidates an in-progress verification pass,
-- including insertion behind its rowid cursor, replacement, and expiry cleanup.
CREATE TABLE continuation_rotation_epoch (
    singleton INTEGER PRIMARY KEY CHECK(singleton=1),
    revision INTEGER NOT NULL CHECK(typeof(revision)='integer' AND revision>=0 AND revision<=9007199254740991)
);
INSERT INTO continuation_rotation_epoch(singleton,revision) VALUES(1,0);
CREATE TRIGGER continuation_rotation_insert AFTER INSERT ON attempt_continuations BEGIN
    UPDATE continuation_rotation_epoch SET revision=revision+1 WHERE singleton=1;
END;
CREATE TRIGGER continuation_rotation_update AFTER UPDATE ON attempt_continuations BEGIN
    UPDATE continuation_rotation_epoch SET revision=revision+1 WHERE singleton=1;
END;
CREATE TRIGGER continuation_rotation_delete AFTER DELETE ON attempt_continuations BEGIN
    UPDATE continuation_rotation_epoch SET revision=revision+1 WHERE singleton=1;
END;
