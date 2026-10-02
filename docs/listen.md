# Listen in any language


Open a story, pick a language and press play: Pulse reads a 30-second briefing,
translated if needed, with the transcript following along word by word.

**The script** is extractive, so nothing is made up: the headline, how widely the
story is covered ("More than 50 sources in 12 languages…"), then up to three
summaries from different outlets, credited by name ("The Hindu: …"). Reports are
the articles closest to the story's centroid, preferring ones already in the
listener's language (no translation needed), skipping syndicated copies and
summaries that repeat each other, and cleaning up wire datelines and cut-off
feed text. Composition is deterministic for a given story state.

**Translation** (DeepL, or a LibreTranslate server) only touches segments not
already in the target language. **Speech** is ElevenLabs Flash v2.5 through the
timestamps endpoint, which returns per-character timings; the API turns those into
word starts (as UTF-16 offsets, what the browser indexes by) so the transcript
highlights the word being spoken, and clicking any word seeks there.

**Cost control.** Both caches are content-addressed: translations by source text,
audio by (provider, model, voice, language, final text), stored in SeaweedFS (S3)
with its metadata in Postgres. Asking again for an unchanged briefing is served
from cache in milliseconds and costs nothing; coverage counts are rounded ("more
than 50") so the text, and the audio, survive a few more sources joining. Spend is
recorded per provider per day, with a daily speech budget, a monthly translation
budget and the ElevenLabs account quota all checked before calling out, and one
synthesis runs at a time so double clicks pay once.

**Fallbacks.** With no ElevenLabs key, an exhausted budget or a provider error,
the briefing still comes back (translated if possible) and the browser's own
speech synthesis reads it, one segment per utterance with the same word
highlighting. With no translator, briefings use only coverage already in the
chosen language, and languages without any are refused with suggestions.

| Setting | Default | |
|---|---|---|
| `DEEPL_API_KEY` | | Free keys (ending `:fx`) use api-free.deepl.com |
| `ELEVENLABS_API_KEY` | | Voices are listed from the account; free tier needs attribution (shown in the player) |
| `PULSE_TTS_DAILY_CHARS` | 3000 | Speech characters per UTC day |
| `PULSE_TRANSLATE_MONTHLY_CHARS` | 400000 | DeepL Free allows 500k |
| `PULSE_BRIEFING_MAX_CHARS` | 520 | About 30 seconds |
| `PULSE_AUDIO_STORE` | `s3` | Or `local` (`data/audio`) |

Verified end to end against local stand-ins for both APIs (real audio from macOS
`say`): a German briefing translated 5 segments and recorded 707 characters in
1.7 s; asking again took 24 ms with zero provider calls. Unit tests cover the
script composition, the provider clients (against an in-process mock), word
timings and budgets.
