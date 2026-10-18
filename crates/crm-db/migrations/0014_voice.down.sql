-- SPDX-License-Identifier: GPL-3.0-only
-- 0014_voice.down: roll back the Calling vertical slice.
--
-- Drops the call_recordings and calls tables (in dependency order).
-- Calls reference client profiles and contacts only; recordings
-- reference the audio asset by id but hold no foreign key (assets are
-- content-addressed and never deleted).

DROP TABLE IF EXISTS call_recordings;
DROP TABLE IF EXISTS calls;
