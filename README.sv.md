<p align="center">
  <img src="https://raw.githubusercontent.com/nikosavola/k-ruoka-mcp/main/.github/logo.svg" alt="k-ruoka-mcp" width="375">
</p>

---

[![en](https://img.shields.io/badge/lang-en-red.svg)](./README.md)
[![fi](https://img.shields.io/badge/lang-fi-blue.svg)](./README.fi.md)
[![sv](https://img.shields.io/badge/lang-sv-yellow.svg)](./README.sv.md)

En MCP-server som hanterar varukorgen för ett [K-Ruoka](https://www.k-ruoka.fi)-konto:
läsa korgen, lägga till varor, ändra antal, ta bort varor och tömma korgen.

Kassan är medvetet utanför omfattningen. Ingenting här kan lägga en order eller spendera
pengar.

> [!IMPORTANT]
> Använd med försiktighet och endast med ditt eget konto. K-Ruokas avtalsvillkor begränsar
> tjänsten till *kundens eget personliga privata bruk*, och Kesko har rätt att begränsa
> eller stänga ett konto efter eget gottfinnande. Risken är din. Läs
> [avsnittet om användarvillkor](#användarvillkor-använd-med-försiktighet) först.

> För fullständig dokumentation, se den [engelska](./README.md) eller den
> [finska](./README.fi.md) versionen.

## Så fungerar det

K-Ruoka har inget offentligt API. Varukorgen ligger bakom ett privat `/kr-api/` som
autentiseras enbart med webbläsarens kakor. Servern styr därför en riktig Chrome via
DevTools-protokollet, sparar en webbläsarprofil på disk och gör varje anrop som ett
`fetch()` inifrån den laddade sidan.

## Krav

- **Google Chrome** i `/usr/bin/google-chrome` (ändra med `K_RUOKA_CHROME`). Inte
  valfritt: kakorna är den enda inloggningsuppgift som API:et godtar.
- `xvfb-run` behövs bara för `login` på en maskin utan skärm.

## Installation

```bash
uvx k-ruoka-mcp login    # en gång, för hand
```

PyPI används enbart som distributionskanal. Paketet innehåller ingen Python-kod, bara den
kompilerade Rust-binären.

Med cargo går det också från crates.io, om Rust-verktygen redan finns på maskinen:

```bash
cargo install k-ruoka-mcp
```

Eller som en färdig binär, utan kompilering:

```bash
cargo binstall k-ruoka-mcp
```

Samma binär går också att hämta för hand från varje release
(`k-ruoka-mcp-<mål>.tar.gz`, `.zip` på Windows); `SHA256SUMS` täcker alla filer.

## Användning

### 1. Logga in en gång

Antingen i en terminal med `uvx k-ruoka-mcp login`, eller be din assistent logga in dig
när servern är registrerad. Den öppnar webbläsaren och ger dig stegen.

En webbläsare öppnas på k-ruoka.fi. Klicka *Kirjaudu* och logga in som vanligt.
Inloggningen upptäcks av sig själv och webbläsaren stängs. Inloggningsuppgifter
automatiseras aldrig och programmet ser dem inte.

Två flikar öppnas: använd den som heter *Tuotteet | K-Ruoka Verkkokauppa*. Den andra
bevakar inloggningen och navigerar bort under dig. På en maskin utan skärm får du
i stället ett `ssh`-kommando och en `chrome://inspect`-adress, så att du kan sköta
inloggningen från din egen dator.

Sessionen sparas i `~/.local/share/k-ruoka-mcp/profile` med rättigheterna `0700`. **Den
innehåller en giltig inloggning, så behandla den som ett lösenord.**

### 2. Registrera servern

```json
{
  "mcpServers": {
    "k-ruoka-cart": {
      "command": "uvx",
      "args": ["k-ruoka-mcp"]
    }
  }
}
```

`serve` är standard, så inget underkommando behövs.

## Verktyg

Alla korgverktyg tar ett `store_id`, eftersom en korg hör till en butik. Till exempel är
`N137` K-Citymarket Helsinki Ruoholahti. `search_stores` hittar ett.

| verktyg | noteringar |
|---|---|
| `search_products` | Endast läsning. Hittar EAN-koder utifrån namn, vilket är vad `add_to_cart` behöver. Sök på finska. |
| `search_stores` | Endast läsning. Hittar det `store_id` som övriga verktyg behöver. |
| `get_cart` | Endast läsning. Enda källan till `itemId`-värden. |
| `add_to_cart` | Med EAN-kod. `quantity` är det slutliga antalet, inte ett tillägg. |
| `update_cart_item` | Sätter exakt antal. 0 tar bort varan. |
| `remove_from_cart` | |
| `clear_cart` | Tömmer korgen. Kan inte ångras. |
| `auth_status` | Om den sparade sessionen fortfarande är inloggad. |
| `start_login` | Öppnar en webbläsare för inloggning och returnerar instruktionerna. |
| `login_status` | `waiting`, `signedIn`, `failed` eller `notStarted`. |
| `cancel_login` | Avbryter en pågående inloggning och stänger webbläsaren. |

Vanligt flöde: `search_stores` en gång för att hitta ett butiks-id, sedan
`search_products` för att omvandla ett namn till en EAN-kod, och därefter `add_to_cart`.
Sök på finska: sortimentet är finskt, så `maito` hittar betydligt mer än `milk`.

En assistent kan sköta inloggningen med `start_login`: den vidarebefordrar
instruktionerna och kontrollerar `login_status` tills du är klar. Korgverktygen är pausade
under en pågående inloggning, eftersom en profil bara rymmer en webbläsare i taget.

Anrop skickas med minst **500 ms** mellanrum, så att en modell som går igenom en inköpslista
inte skickar en skur av förfrågningar.

## Användarvillkor: använd med försiktighet

Detta använder ett privat API via din egen inloggade session. Betrakta det som något att
vara försiktig med.

K-Ruokas
[avtalsvillkor](https://www.k-ruoka.fi/artikkelit/kayttoehdot/k-ruoka-fi-palvelun-sopimusehdot)
(15.6.2026) begränsar användningen av tjänstens material till kundens eget personliga
privata bruk, och kontohavaren ansvarar för allt som görs med hens uppgifter. Kesko har
rätt att hindra användning av tjänsten eller stänga ett konto efter eget gottfinnande.

Verktyget är byggt för att hålla sig inom det: **ett konto, ditt eget, och ingenting annat
än din egen korg.**

- Inga andra användares uppgifter läses, och ingenting samlas in i bulk.
- **Ingen kassa.** Ingen order kan läggas och inga pengar spenderas.
- Förfrågningarna är begränsade och volymen är klart lägre än vanlig surfning.
- Ingenting vidaredistribueras eller säljs.

**Läs de gällande villkoren själv och fatta ditt eget beslut.** Villkoren kan ändras, och
hur de tillämpas på en människostyrd assistent som arbetar med ditt eget konto är din
bedömning som kontohavare. Detta är inte juridisk rådgivning.

## Varumärken

Inte kopplat till eller godkänt av Kesko Oyj. *K-Ruoka*, *K-Plussa*, *K-Citymarket*,
*Pirkka* och *Kesko* är varumärken som tillhör Kesko Oyj och används här endast för att
beskriva vad programvaran fungerar med.

## Utveckling

```bash
git clone https://github.com/nikosavola/k-ruoka-mcp.git
cd k-ruoka-mcp
just install
just test
```

Se [CONTRIBUTING.md](CONTRIBUTING.md).
