CREATE TABLE collections (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    code       TEXT    NOT NULL UNIQUE,
    name       TEXT    NOT NULL,
    created_at INTEGER NOT NULL
);

CREATE TABLE users (
    id                   INTEGER PRIMARY KEY,
    period_secs          INTEGER NOT NULL,
    paused               INTEGER NOT NULL DEFAULT 0,
    next_quiz_at         INTEGER NOT NULL,
    active_collection_id INTEGER REFERENCES collections(id) ON DELETE SET NULL
);

CREATE TABLE memberships (
    user_id       INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    collection_id INTEGER NOT NULL REFERENCES collections(id) ON DELETE CASCADE,
    joined_at     INTEGER NOT NULL,
    PRIMARY KEY (user_id, collection_id)
);
CREATE INDEX memberships_collection ON memberships(collection_id);

CREATE TABLE words (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    collection_id INTEGER NOT NULL REFERENCES collections(id) ON DELETE CASCADE,
    term          TEXT    NOT NULL,
    term_norm     TEXT    NOT NULL,
    UNIQUE (collection_id, term_norm)
);

CREATE TABLE translations (
    id        INTEGER PRIMARY KEY AUTOINCREMENT,
    word_id   INTEGER NOT NULL REFERENCES words(id) ON DELETE CASCADE,
    text      TEXT    NOT NULL,
    text_norm TEXT    NOT NULL,
    UNIQUE (word_id, text_norm)
);

CREATE TABLE quizzes (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    user_id       INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    word_id       INTEGER NOT NULL REFERENCES words(id) ON DELETE CASCADE,
    options       TEXT    NOT NULL,
    correct_index INTEGER NOT NULL,
    message_id    INTEGER,
    sent_at       INTEGER NOT NULL,
    answered_at   INTEGER,
    chosen_index  INTEGER,
    is_correct    INTEGER
);
CREATE UNIQUE INDEX quizzes_one_open ON quizzes(user_id) WHERE answered_at IS NULL;
CREATE INDEX quizzes_user_word ON quizzes(user_id, word_id);
