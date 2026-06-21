# gemini-oauth-cli

Klient CLI do **Google Gemini** napisany w Rust, który używa **tylko logowania
OAuth** (zwykłe konto Google) — **bez API key**. Pod spodem korzysta z tego
samego backendu **Code Assist** (`cloudcode-pa.googleapis.com`), z którego
korzysta oficjalne, otwartoźródłowe `gemini-cli`. Dzięki temu zalogowanie się
przez przeglądarkę wystarcza, by wołać model „jak API”.

> To jest klient interoperacyjny do prywatnego użytku z własnym kontem Google.
> Obowiązują Cię warunki korzystania z usług Google / Gemini.

## Jak to działa

1. **OAuth (loopback)** — `gemini login` uruchamia lokalny serwer na
   `127.0.0.1:<port>`, otwiera ekran zgody Google, przechwytuje kod z
   przekierowania i wymienia go na tokeny. Token odświeżany jest automatycznie.
   Używany jest publiczny klient OAuth z `gemini-cli` (scope `cloud-platform`).
2. **Discovery projektu** — przy pierwszym zapytaniu wołane są
   `loadCodeAssist` + `onboardUser`, które ustalają/provisionują projekt Code
   Assist (darmowy tier dla kont osobistych). Id projektu jest cache'owane.
3. **Generowanie** — zapytania trafiają do `:generateContent` /
   `:streamGenerateContent` w kopercie `{ model, project, request }`, a
   odpowiedź jest rozpakowywana z `{ "response": ... }`.

Tokeny i cache projektu zapisywane są w `~/.config/gemini-oauth-cli/store.json`
(uprawnienia `0600` na Unix).

## Budowanie

```bash
cargo build --release
# binarka: target/release/gemini
```

## Użycie

```bash
# 1) jednorazowe logowanie przez przeglądarkę
gemini login

# 2) zadaj pytanie (domyślnie streaming)
gemini ask "Wyjaśnij borrow checker w jednym zdaniu"

# wybór modelu + instrukcja systemowa + temperatura
gemini ask -m gemini-2.5-pro -s "Odpowiadaj po polsku, zwięźle" -t 0.2 "Co to jest WAL?"

# użycie w pipe (jak API) — prompt ze stdin
echo "Streść ten tekst:" | cat - artykul.txt | gemini ask --no-stream

# interaktywny czat z pamięcią kontekstu
gemini chat -m gemini-2.5-pro

# stan logowania / wykryty projekt
gemini status

# wyloguj (usuwa lokalne tokeny i cache)
gemini logout
```

### Komendy w trybie `chat`
- `/reset` — wyczyść kontekst rozmowy
- `/exit` lub `/quit` — wyjście

## Zmienne środowiskowe

- `GOOGLE_CLOUD_PROJECT` — wymuś konkretny projekt GCP (np. konta płatne /
  Workspace), pomijając auto-provisioning darmowego tieru.
- `GEMINI_OAUTH_CLIENT_ID` / `GEMINI_OAUTH_CLIENT_SECRET` — nadpisz domyślny
  publiczny klient OAuth własnym (np. utworzonym w Google Cloud Console jako
  „Desktop app”).

## Modele

Przekazywane są bezpośrednio do backendu, np.:
- `gemini-2.5-flash` (domyślny)
- `gemini-2.5-pro`

## Uwagi

- Pierwsze logowanie wymaga zgody `prompt=consent`, aby otrzymać
  `refresh_token` (dzięki temu nie trzeba logować się za każdym razem).
- Jeśli odświeżanie tokenu zwróci błąd, uruchom ponownie `gemini login`.
