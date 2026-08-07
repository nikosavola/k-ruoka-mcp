<p align="center">
  <img src="https://raw.githubusercontent.com/nikosavola/k-ruoka-mcp/main/.github/logo.svg" alt="k-ruoka-mcp" width="375">
</p>

---

[![en](https://img.shields.io/badge/lang-en-red.svg)](./README.md)
[![fi](https://img.shields.io/badge/lang-fi-blue.svg)](./README.fi.md)
[![sv](https://img.shields.io/badge/lang-sv-yellow.svg)](./README.sv.md)
[![codecov](https://codecov.io/gh/nikosavola/k-ruoka-mcp/graph/badge.svg)](https://codecov.io/gh/nikosavola/k-ruoka-mcp)

MCP-palvelin, joka hallinnoi yhden [K-Ruoka](https://www.k-ruoka.fi)-tilin ostoskoria:
lukee korin, lisää tuotteita, muuttaa määriä, poistaa tuotteita ja tyhjentää korin.

Kassalle siirtyminen on jätetty tarkoituksella pois. Mikään tässä ei voi tehdä tilausta
eikä käyttää rahaa.

> [!IMPORTANT]
> Käytä harkiten ja vain omalla tililläsi. K-Ruokan sopimusehdot rajaavat palvelun
> *asiakkaan omaan henkilökohtaiseen yksityiseen käyttöön*, ja Keskolla on oikeus
> rajoittaa tai sulkea tili harkintansa mukaan. Riski on sinun. Lue
> [käyttöehdot-osio](#käyttöehdot-käytä-harkiten) ensin.

## Miten tämä toimii

K-Ruokalla ei ole julkista rajapintaa. Ostoskori on yksityisen `/kr-api/`-rajapinnan
takana, joka tunnistautuu pelkillä selaimen evästeillä. Siksi palvelin ohjaa aitoa
Chromea DevTools-protokollalla, pitää selainprofiilia levyllä ja tekee jokaisen kutsun
`fetch()`-kutsuna ladatun sivun sisältä. Selain liittää evästeet itse.

Sivusto on Cloudflaren takana. Läpi pääsemiseen riittää yksi asia: User-Agent, joka ei
sisällä merkkijonoa `HeadlessChrome`. Ei stealth-lisäosia eikä haasteiden ratkomista.

## Vaatimukset

- **Google Chrome** polussa `/usr/bin/google-chrome` (ohita: `K_RUOKA_CHROME`). Ei
  valinnainen: evästeet ovat ainoa tunniste, jonka rajapinta hyväksyy.
- `xvfb-run` vain `login`-komentoon näytöttömällä koneella.
- Rust 1.88+ vain jos käännät itse.

## Asennus

Julkaistu PyPI:hin valmiiksi käännettynä binäärinä, joten `uvx` hakee ja ajaa sen ilman
Rust-työkaluja:

```bash
uvx k-ruoka-mcp login    # kerran, käsin
```

PyPI on tässä pelkkä jakelukanava. Paketissa ei ole Python-koodia:
[maturin](https://github.com/PyO3/maturin)in `bin`-sidonta asettaa käännetyn ohjelman
suoraan ympäristön `bin/`-hakemistoon.

Cargon kanssa myös crates.iosta, jos Rust-työkalut ovat jo koneella:

```bash
cargo install k-ruoka-mcp
```

Tai valmiina binäärinä ilman kääntämistä:

```bash
cargo binstall k-ruoka-mcp
```

Sama binääri löytyy myös käsin jokaisesta julkaisusta
(`k-ruoka-mcp-<kohde>.tar.gz`, Windowsilla `.zip`), ja `SHA256SUMS` kattaa kaikki
tiedostot.

## Käyttöönotto

### 1. Kirjaudu sisään kerran

Joko terminaalissa:

```bash
uvx k-ruoka-mcp login
```

Tai kun palvelin on rekisteröity (kohta 2), pyydä avustajaa kirjaamaan sinut sisään. Se
avaa selaimen ja antaa ohjeet.

Kummin päin tahansa selain avautuu k-ruoka.fi-sivulle. Paina *Kirjaudu*, kirjaudu
normaalisti, ja siinä se: kirjautuminen huomataan itsestään ja selain sulkeutuu. Tunnuksia
ei automatisoida eikä tämä ohjelma näe niitä.

Kaksi asiaa on hyvä tietää:

- **Auki on kaksi välilehteä.** Käytä sitä, jonka otsikko on *Tuotteet | K-Ruoka
  Verkkokauppa*. Toinen (`[k-ruoka-mcp] poller`) tarkkailee kirjautumista ja navigoi pois
  alta.
- **Näytöttömällä koneella**, palvelimella tai Dockerissa, saat ikkunan sijaan `ssh`
  -komennon ja `chrome://inspect`-osoitteen, joilla käytät selainta omalta koneeltasi.
  Seuraa tulostettuja ohjeita; ne ovat täsmälliset.

Kirjautuminen tallennetaan hakemistoon `~/.local/share/k-ruoka-mcp/profile` (ohita:
`K_RUOKA_PROFILE`). **Käsittele hakemistoa kuin salasanaa.** Aja `login` uudelleen, kun
istunto vanhenee.

### 2. Rekisteröi palvelin

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

Alikomentoa ei tarvita, koska `serve` on oletus. Chrome käynnistyy vasta ensimmäisellä
työkalukutsulla, joten palvelin itse käynnistyy heti.

## Työkalut

Jokainen korityökalu ottaa `store_id`:n, koska kori kuuluu kauppaan. Esimerkiksi `N137` on
K-Citymarket Helsinki Ruoholahti. `search_stores` löytää tunnisteen.

| työkalu | huomiot |
|---|---|
| `search_products(store_id, query, limit?)` | Vain luku. Etsii EAN-koodit nimellä, ja juuri niitä `add_to_cart` tarvitsee. Hae suomeksi. |
| `search_stores(query, limit?)` | Vain luku. Löytää `store_id`:n, jonka muut työkalut tarvitsevat. |
| `get_cart(store_id)` | Vain luku. Ainoa lähde `itemId`-arvoille. |
| `add_to_cart(store_id, ean, quantity?, unit?, ...)` | EAN-koodilla. `quantity` on lopullinen määrä, ei lisäys. |
| `update_cart_item(store_id, item_id, quantity, unit?)` | Asettaa täsmällisen määrän. 0 poistaa. |
| `remove_from_cart(store_id, item_id)` | |
| `clear_cart(store_id)` | Tyhjentää korin. Ei peruttavissa. |
| `auth_status(store_id)` | Onko tallennettu istunto vielä kirjautunut. |
| `start_login(port?)` | Avaa selaimen kirjautumista varten ja palauttaa ohjeet käyttäjälle. |
| `login_status()` | `waiting`, `signedIn`, `failed` tai `notStarted`. |
| `cancel_login()` | Keskeyttää kirjautumisen ja sulkee selaimen. |

Kaksi asiaa kannattaa tietää:

- **`item_id` ei ole EAN.** Se on korin oma tunniste, joka syntyy vasta kun tuote on
  korissa. K-Ruoka vastaa tuntemattomaan tunnisteeseen `200` muuttamatta mitään, joten
  hiljainen tyhjäkäynti näyttäisi onnistumiselta. Siksi molemmat tarkistavat tunnisteen.
- **`add_to_cart` asettaa määrän, ei kasvata sitä.** Kaksi kutsua arvolla `quantity: 1`
  jättää koriin yhden. Mitattu, ei arvattu.

Tavallinen kulku: `search_stores` kerran kaupan tunnisteen löytämiseksi, sitten
`search_products` muuttamaan nimen EAN-koodiksi, ja lopuksi `add_to_cart`.

- **Hae suomeksi.** Valikoima on suomenkielinen, joten `maito` löytää paljon enemmän kuin
  `milk`. Tulokset ovat kauppakohtaisia: hinta ja saatavuus vaihtelevat kauppojen välillä.
- **Tarkista hakutuloksen `isAvailable`.** Tuote voi olla valikoimassa mutta ei ostettavissa
  kyseisestä kaupasta, ja `add_to_cart` hyväksyy EAN-koodin kumminkin päin.

### Kirjautuminen avustajan kautta

`start_login` antaa mallin hoitaa kirjautumisen sen sijaan, että se käskisi avaamaan
terminaalin. Se palauttaa samat ohjeet, jotka `login` tulostaa, ja ne vaihtelevat koneen
mukaan, joten avustajan kannattaa välittää ne sellaisenaan.

- **Korityökalut ovat tauolla kirjautumisen ajan**, ja ne kertovat sen. Yksi selain per
  profiili, joten palvelin lainaa omansa kirjautumiselle. `cancel_login` ottaa sen
  takaisin.
- **Dockerissa julkaise debug-portti heti käynnistyksessä** (`-p 127.0.0.1:9222:9222`) ja
  anna `start_login`in käyttää oletusporttia. Käynnissä oleva kontti ei voi julkaista
  porttia jälkikäteen.

### Kutsutaajuuden rajoitus

Pyynnöt lähetetään vähintään **500 ms** välein koko prosessissa.

Kyse ei ole suorituskyvystä vaan muodosta. MCP-asiakkaat kutsuvat työkaluja rinnakkain, ja
malli voi käydä ostoslistaa läpi tiukassa silmukassa. Ilman välistystä siitä tulisi
purske, joka ei muistuta ihmisen tapaa käyttää verkkokauppaa. Ensimmäistä pyyntöä ei
viivytetä.

```bash
K_RUOKA_MIN_REQUEST_INTERVAL_MS=1000   # varovaisempi
K_RUOKA_MIN_REQUEST_INTERVAL_MS=0      # pois
```

## Käyttöehdot: käytä harkiten

Tämä käyttää yksityistä rajapintaa oman kirjautuneen selainistuntosi kautta. Suhtaudu
siihen varovaisuutta vaativana asiana.

K-Ruokan
[sopimusehdot](https://www.k-ruoka.fi/artikkelit/kayttoehdot/k-ruoka-fi-palvelun-sopimusehdot)
(15.6.2026) rajaavat aineiston käytön

> Asiakkaan omaan henkilökohtaiseen yksityiseen käyttöön

ja asettavat tilin haltijan vastuuseen kaikesta, mitä hänen tunnuksillaan tehdään.
Keskolla on oikeus estää palvelun käyttö tai sulkea tili oman harkintansa mukaan.

Työkalu on rakennettu pysymään sen sisällä: **yksi tili, sinun oma, eikä mitään muuta kuin
sinun oma korisi.**

- Muiden tietoja ei lueta, eikä mitään kerätä massana.
- **Ei kassatoimintoa.** Tilausta ei voi tehdä eikä rahaa käyttää.
- Pyyntöjä rajoitetaan, ja määrä on selvästi tavallista selailua pienempi.
- Mitään ei jaeta eteenpäin eikä myydä.

**Lue voimassa olevat ehdot itse ja tee oma päätöksesi.** Ehdot voivat muuttua, ja se miten
ne soveltuvat ihmisen ohjaamaan avustajaan omalla tilillä on tilin haltijan arvioitava.
Riski on oma K-Plussa-tilisi. Tämä ei ole oikeudellista neuvontaa.

## Tavaramerkit

Ei liity Kesko Oyj:hin eikä ole sen hyväksymä. *K-Ruoka*, *K-Plussa*, *K-Citymarket*,
*Pirkka* ja *Kesko* ovat Kesko Oyj:n tavaramerkkejä, ja niitä käytetään vain kuvaamaan
mihin ohjelmisto liittyy. K-Ruokan sisältöä ei jaeta eteenpäin.

## Kehittäminen

```bash
git clone https://github.com/nikosavola/k-ruoka-mcp.git
cd k-ruoka-mcp
just install
just test
```

Katso [CONTRIBUTING.md](CONTRIBUTING.md).
