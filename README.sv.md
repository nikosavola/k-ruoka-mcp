# k-ruoka-mcp

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

## Användning

1. Logga in en gång för hand med `uvx k-ruoka-mcp login`. Inloggningsuppgifter
   automatiseras aldrig och programmet ser dem inte.
1. Registrera servern hos din MCP-klient:

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

Sessionen sparas i `~/.local/share/k-ruoka-mcp/profile` med rättigheterna `0700`. **Den
innehåller en giltig inloggning, så behandla den som ett lösenord.**

## Verktyg

Alla verktyg tar ett `store_id`, eftersom en korg hör till en butik. Till exempel är `N137`
K-Citymarket Helsinki Ruoholahti.

| verktyg | noteringar |
|---|---|
| `get_cart` | Endast läsning. Enda källan till `itemId`-värden. |
| `add_to_cart` | Med EAN-kod. `quantity` är det slutliga antalet, inte ett tillägg. |
| `update_cart_item` | Sätter exakt antal. 0 tar bort varan. |
| `remove_from_cart` | |
| `clear_cart` | Tömmer korgen. Kan inte ångras. |
| `auth_status` | Om den sparade sessionen fortfarande är inloggad. |

Servern kan inte söka efter produkter. Den tar en färdig EAN-kod.

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
