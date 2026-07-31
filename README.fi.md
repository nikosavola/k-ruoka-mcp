# k-ruoka-mcp

[![en](https://img.shields.io/badge/lang-en-red.svg)](./README.md)
[![fi](https://img.shields.io/badge/lang-fi-blue.svg)](./README.fi.md)
[![sv](https://img.shields.io/badge/lang-sv-yellow.svg)](./README.sv.md)

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
- Rust 1.94+ vain jos käännät itse.

## Asennus

Julkaistu PyPI:hin valmiiksi käännettynä binäärinä, joten `uvx` hakee ja ajaa sen ilman
Rust-työkaluja:

```bash
uvx k-ruoka-mcp login    # kerran, käsin
```

PyPI on tässä pelkkä jakelukanava. Paketissa ei ole Python-koodia:
[maturin](https://github.com/PyO3/maturin)in `bin`-sidonta asettaa käännetyn ohjelman
suoraan ympäristön `bin/`-hakemistoon.

## Käyttöönotto

### 1. Kirjaudu sisään kerran käsin

```bash
uvx k-ruoka-mcp login
```

Tunnuksia ja monivaiheista tunnistautumista ei automatisoida, eikä tämä ohjelma näe niitä.
Komento avaa selaimen ja odottaa, kunnes K-Ruoka raportoi kirjautuneesta tilistä. Paina
*Kirjaudu* ja kirjaudu normaalisti.

Avautuu kaksi välilehteä. Käytä sitä, jonka otsikko on *Tuotteet | K-Ruoka Verkkokauppa*.
Toinen (`[k-ruoka-mcp] poller`) on tämä prosessi tarkistamassa kirjautumista, ja se
navigoidaan pois alta muutaman sekunnin välein.

Näytöttömällä koneella komento käynnistyy uudelleen `xvfb-run`in alla ja tulostaa ohjeet
selaimen käyttöön `ssh -L` -tunnelin ja `chrome://inspect`-sivun kautta.

Istunto tallennetaan hakemistoon `~/.local/share/k-ruoka-mcp/profile` (oikeudet `0700`,
ohita: `K_RUOKA_PROFILE`). **Se sisältää voimassa olevan kirjautumisen, joten käsittele
sitä kuin salasanaa.**

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
työkalukutsulla.

## Työkalut

Jokainen työkalu ottaa `store_id`:n, koska kori kuuluu kauppaan. Esimerkiksi `N137` on
K-Citymarket Helsinki Ruoholahti.

| työkalu | huomiot |
|---|---|
| `get_cart(store_id)` | Vain luku. Ainoa lähde `itemId`-arvoille. |
| `add_to_cart(store_id, ean, quantity?, unit?, ...)` | EAN-koodilla. `quantity` on lopullinen määrä, ei lisäys. |
| `update_cart_item(store_id, item_id, quantity, unit?)` | Asettaa täsmällisen määrän. 0 poistaa. |
| `remove_from_cart(store_id, item_id)` | |
| `clear_cart(store_id)` | Tyhjentää korin. Ei peruttavissa. |
| `auth_status(store_id)` | Onko tallennettu istunto vielä kirjautunut. |

Kaksi asiaa kannattaa tietää:

- **`item_id` ei ole EAN.** Se on korin oma tunniste, joka syntyy vasta kun tuote on
  korissa. K-Ruoka vastaa tuntemattomaan tunnisteeseen `200` muuttamatta mitään, joten
  hiljainen tyhjäkäynti näyttäisi onnistumiselta. Siksi molemmat tarkistavat tunnisteen.
- **`add_to_cart` asettaa määrän, ei kasvata sitä.** Kaksi kutsua arvolla `quantity: 1`
  jättää koriin yhden. Mitattu, ei arvattu.

Palvelin ei osaa etsiä tuotteita. Se ottaa valmiin EAN-koodin.

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
