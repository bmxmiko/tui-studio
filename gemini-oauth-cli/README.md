# gemini-oauth-cli

Klient CLI w Rust do **Google Gemini** **oraz Anthropic Claude**, który używa
**tylko logowania OAuth** — **bez API key**. Gemini działa przez backend
**Code Assist** (jak `gemini-cli`); Claude przez przepływ OAuth **Claude Code**
(subskrypcja Pro/Max). Wybór dostawcy flagą `-p/--provider` (`gemini` domyślnie,
albo `claude`).

> Klient interoperacyjny do prywatnego użytku z **własnym** kontem.
> Obowiązują Cię warunki korzystania z usług Google / Anthropic.

> ⚠️ **Uwaga o Claude:** dostęp do Messages API przez OAuth Claude Code wymaga
> nagłówka beta `oauth-2025-04-20` oraz tożsamości „You are Claude Code” jako
> pierwszego bloku systemowego. Anthropic **aktywnie ogranicza** użycie OAuth
> przez klientów innych niż Claude Code — ten tryb jest znacznie bardziej kruchy
> niż Gemini i może przestać działać lub naruszać ToS. Używaj świadomie.

## Funkcje

- ✅ Logowanie **OAuth** (bez API key), automatyczne odświeżanie tokenu — Gemini i Claude
- ✅ **Wybór dostawcy** (`-p gemini` / `-p claude`)
- ✅ **Zmiana modelu** (`-m`, np. `gemini-2.5-pro`, `claude-sonnet-4-5`)
- ✅ **Upload plików** — obrazy / PDF / tekst (base64; `inlineData` / bloki `image`/`document`)
- ✅ **Poziom myślenia** (`--thinking`) + podgląd toku rozumowania (`--show-thoughts`)
- ✅ Streaming odpowiedzi (SSE) oraz tryb pipe (stdin) „jak API”
- ✅ Interaktywny czat z pamięcią kontekstu i dołączaniem plików
- ✅ **Gemy** — lokalne, nazwane persony (instrukcja + domyślny model/temperatura/myślenie/provider)
- ❌ Deep Research — funkcja aplikacji webowych, niedostępna przez te endpointy

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

Dla **Claude** flow jest inny: `gemini -p claude login` używa OAuth 2.0 z PKCE
(klient Claude Code), otwiera ekran zgody i prosi o **wklejenie kodu**
autoryzacyjnego. Następnie żądania idą do `https://api.anthropic.com/v1/messages`
z `Authorization: Bearer`, nagłówkiem `anthropic-beta: oauth-2025-04-20,…`
i tożsamością Claude Code w prompcie systemowym.

Tokeny obu dostawców zapisywane są w `~/.config/gemini-oauth-cli/store.json`
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

# upload plików (obraz / PDF / tekst) — można podać wiele -f
gemini ask -f diagram.png -f notatki.pdf "Co przedstawiają te pliki?"
gemini ask -f zrzut.png    # sam plik, bez promptu też zadziała

# poziom myślenia (Gemini 2.5): budżet tokenów + podgląd toku rozumowania
gemini ask -m gemini-2.5-pro --thinking 8192 --show-thoughts "Rozwiąż tę zagadkę logiczną: ..."
gemini ask --thinking 0 "Szybka odpowiedź bez myślenia"   # 0 = wyłącz, -1 = dynamiczny

# użycie w pipe (jak API) — prompt ze stdin
echo "Streść ten tekst:" | cat - artykul.txt | gemini ask --no-stream

# interaktywny czat z pamięcią kontekstu (z myśleniem)
gemini chat -m gemini-2.5-pro --thinking -1 --show-thoughts

# stan logowania obu dostawców
gemini status

# wyloguj wybranego dostawcę (usuwa jego tokeny)
gemini logout              # gemini
gemini -p claude logout    # claude
```

### Claude (`-p claude`)

```bash
# logowanie OAuth (PKCE, wklejasz kod autoryzacyjny ze strony)
gemini -p claude login

# pytanie (domyślny model: claude-sonnet-4-5)
gemini -p claude ask "Napisz haiku o Rust"

# inny model + analiza pliku
gemini -p claude ask -m claude-opus-4-1 -f zrzut.png "Co tu nie gra?"

# extended thinking (budżet >0 włącza, min 1024) + podgląd rozumowania
gemini -p claude ask --thinking 4096 --show-thoughts "Rozwiąż tę zagadkę: ..."

# czat
gemini -p claude chat
```

### Komendy w trybie `chat`
- `/file <ścieżka>` — dołącz plik do następnej wiadomości
- `/reset` — wyczyść kontekst rozmowy
- `/exit` lub `/quit` — wyjście

### Gemy (własne persony)

Gem to nazwany zestaw: instrukcja systemowa + opcjonalne domyślne ustawienia
(model, dostawca, temperatura, myślenie). To lokalny odpowiednik „Gemów” z
aplikacji Gemini (Twoich Gemów z gemini.google.com **nie da się** pobrać tą
drogą — żyją w aplikacji webowej za inną autoryzacją).

```bash
# utwórz gema (instrukcja wprost lub z pliku)
gemini gem add recenzent \
  --system "Jesteś surowym recenzentem kodu. Wypunktuj ryzyka i poprawki." \
  --model gemini-2.5-pro --temperature 0.3 --description "Code review"

gemini gem add tlumacz --system-file ./prompty/tlumacz.txt --provider claude

# lista / szczegóły / usuwanie
gemini gem list
gemini gem show recenzent
gemini gem remove recenzent

# użyj gema (flagi z linii poleceń nadpisują ustawienia gema)
gemini ask -g recenzent -f main.rs "Sprawdź ten plik"
gemini chat -g tlumacz
```

Kolejność ustawień: **flaga CLI > gem > domyślne**. Jeśli gem ma ustawiony
`provider`, zostanie użyty, o ile nie podasz `-p` jawnie.

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
