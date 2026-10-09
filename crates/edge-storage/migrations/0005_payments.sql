-- PI.Edge local schema, version 5: payments become a bounded context.
--
-- The v1 `payments` table modelled a cash tender that was created and
-- committed in one step. That shape cannot express a provider interaction, an
-- unknown outcome or a retry, so it is replaced rather than extended: the old
-- table is dropped and the new model takes its name (ADR 016).
--
-- Dropping it discards any pre-existing payment rows. That is deliberate and
-- safe here: the cash path it served is removed in the same milestone, and no
-- installation has carried production money through it.
PRAGMA defer_foreign_keys = ON;

DROP TABLE payments;

-- The commercial intention to cover part of a sale with one tender.
CREATE TABLE payments (
    payment_id        TEXT    PRIMARY KEY,
    transaction_id    TEXT    NOT NULL REFERENCES retail_transactions (transaction_id)
                              ON DELETE CASCADE,
    tender_type       TEXT    NOT NULL CHECK (tender_type IN ('CASH', 'CARD')),
    -- Who processes it. Set exactly for tenders settled through a provider:
    -- cash has none, which is why cash has no attempts either.
    provider_kind     TEXT    CHECK (provider_kind IS NULL OR provider_kind IN ('SIMULATOR')),
    -- What this tender was asked to cover. For cash, what the customer handed
    -- over, so an overpayment is visible rather than inferred.
    requested_minor   INTEGER NOT NULL CHECK (requested_minor > 0),
    -- What a provider approved. NULL for cash and for unresolved payments.
    authorized_minor  INTEGER CHECK (authorized_minor IS NULL OR authorized_minor >= 0),
    -- What is applied against the sale's debt. The difference from
    -- requested_minor is change, and only cash can produce it.
    committed_minor   INTEGER NOT NULL DEFAULT 0 CHECK (committed_minor >= 0),
    currency          TEXT    NOT NULL,
    status            TEXT    NOT NULL CHECK (status IN
                                  ('CREATED', 'IN_PROGRESS', 'COMMITTED', 'FAILED', 'REVERSED')),
    created_at        TEXT    NOT NULL,
    updated_at        TEXT    NOT NULL,
    CHECK ((tender_type = 'CASH') = (provider_kind IS NULL)),
    CHECK (committed_minor <= requested_minor),
    -- Only a committed payment has money applied.
    CHECK (status = 'COMMITTED' OR committed_minor = 0)
);

CREATE INDEX ix_payments_transaction ON payments (transaction_id, created_at);

-- Every unresolved payment of a sale, which is what blocks finalization.
CREATE INDEX ix_payments_unresolved
    ON payments (transaction_id, status)
    WHERE status IN ('CREATED', 'IN_PROGRESS');

-- One concrete interaction with a provider. Append-only: a retry adds a row,
-- it never rewrites the previous one (ADR 016).
CREATE TABLE payment_attempts (
    attempt_id         TEXT    PRIMARY KEY,
    payment_id         TEXT    NOT NULL REFERENCES payments (payment_id) ON DELETE CASCADE,
    -- 1-based, never reused within a payment.
    attempt_number     INTEGER NOT NULL CHECK (attempt_number > 0),
    status             TEXT    NOT NULL CHECK (status IN
                                   ('CREATED', 'PROCESSING', 'AUTHORIZED', 'UNKNOWN',
                                    'FAILED', 'REVERSAL_PENDING', 'REVERSED')),
    requested_minor    INTEGER NOT NULL CHECK (requested_minor > 0),
    authorized_minor   INTEGER CHECK (authorized_minor IS NULL OR authorized_minor >= 0),
    currency           TEXT    NOT NULL,
    provider_kind      TEXT    NOT NULL CHECK (provider_kind IN ('SIMULATOR')),
    -- Minted by the core and committed BEFORE the provider is called, so a
    -- charge that happened can always be asked about (ADR 020).
    correlation_id     TEXT    NOT NULL UNIQUE,
    -- The provider's own identifier, recorded when it answers.
    provider_reference TEXT,
    -- Stable refusal reason. Never a raw provider payload.
    failure_code       TEXT,
    created_at         TEXT    NOT NULL,
    updated_at         TEXT    NOT NULL,
    UNIQUE (payment_id, attempt_number),
    -- An authorization carries an amount.
    CHECK (status <> 'AUTHORIZED' OR authorized_minor IS NOT NULL)
);

-- One provider operation can never be recorded under two attempts.
CREATE UNIQUE INDEX ux_payment_attempts_provider_reference
    ON payment_attempts (provider_kind, provider_reference)
    WHERE provider_reference IS NOT NULL;

CREATE INDEX ix_payment_attempts_payment ON payment_attempts (payment_id, attempt_number);

-- Nothing above stores a PAN, a CVV, track data, a terminal payload or any
-- other cardholder data. PI.Edge never receives it: the provider port carries
-- amounts, correlation ids and stable status codes only.
