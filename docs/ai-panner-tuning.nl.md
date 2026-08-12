# AI-cameratracking & panner-afstelling

Naslagwerk voor de sectie **AI Tracking** in het Export-dialoogvenster
(`reco-gui`). Legt uit wat elke instelling precies doet en hoe je hem
afstemt voor voetbal. Gebaseerd op `crates/reco-autocam/src/panners/field.rs`
en `crates/reco-autocam/src/tracking_mode.rs` - niet geraden.

## Aanbevolen startinstellingen

Eind-tot-eind gevalideerd (2026-08-12) tegen een echte hoek-uitbraak-clip
- onderstaande waarden lossen "camera volgt de bal niet de hoek in /
bij een uitbraak" op, voor zover instellingen dat alleen kunnen. Begin
hier, stem niet vanaf nul handmatig af. Deze staan ook per calibratie
opgeslagen zodra je op **Save calibration** klikt in het
Export-dialoogvenster - zie
[`Calibration::autocam_defaults`](../crates/reco-core/src/calibration.rs).

**Model & hoofdniveau**

| Instelling | Waarde |
|---|---|
| Model | huidige beste checkpoint - `yolo26n_v2` in productie op moment van schrijven; `yolo26n_v3`/`yolo26s_v3` getraind en ONNX-geëxporteerd maar nog niet in de app getest, zie `YOLO26_Training.md` |
| Tracking mode | `field` |
| Detect every N frames | `3` |
| Ball anchor range | `0,3-0,5 rad` (standaard `0,20`) |
| Style preset | `action` (als basis, daarna hieronder overschrijven) |
| Framing | `action` |
| Pitch - Lock (alleen horizontaal) | uit |
| Lookahead (soepelheid) | `0,5s` |
| Reduce lookahead memory (8-bit) | uit (alleen bij een VRAM-error) |

**Advanced panner**

| Instelling | Waarde | Waarom |
|---|---|---|
| Cluster mode | `trimmed_mean` | fixt de freeze van meerdere seconden bij balloze fases |
| Dead-zone | `0,05-0,08 rad` | nodig samen met `trimmed_mean`, samen getest |
| Ball weight | `0,35` | `1,0` gaf zichtbare wobble, ongeacht modelkwaliteit |
| Ball reach | `1,0 rad` (standaard `0,5`) | laat de panner richting een echt geïsoleerde bal trekken i.p.v. 'm te negeren |
| FOV Wide | `65-70°` (standaard van de `action` preset is `48°`) | zonder dit clamped de bal-reach-verbredingslogica voordat het beeld echt breed genoeg kan worden |
| FOV Tight / Default | laat op preset-waarde (`20°` / `34°`) | niet apart getuned |
| Cluster bandwidth | laat op preset-waarde | niet apart getuned |

De drie bal-gerelateerde instellingen filteren voor elkaar, in deze
volgorde: **Ball anchor range → Ball reach → FOV Wide**. Ball anchor
range bepaalt of de *tracker* een verre detectie überhaupt accepteert;
Ball reach bepaalt of de *panner* 'm de aim mag laten trekken; FOV Wide
bepaalt of het *beeld* daadwerkelijk breed genoeg kan worden om het te
tonen. Alleen één van de drie verhogen lost een gemiste uitbraak niet
volledig op - zie de bullet "Camera volgt de bal niet de hoek in"
verderop voor de volledige onderbouwing en hoe elk is geverifieerd (niet
alleen aanbevolen op basis van een gok).

## Instellingen op hoofdniveau

**Tracking mode** - `field` (standaard): volgt de groep spelers + de bal
samen; valt terug op alleen-bal als het model geen spelersklasse heeft.
`ball`: volgt alleen de bal (hogere zekerheidsdrempel, strengere
max-sprong-grens). `sweep`: geen AI - een vaste sinusvormige links-rechts
pan, alleen bruikbaar als debug/basislijn-modus.

**Detect every N frames** - hoe vaak de detector daadwerkelijk draait;
tussenliggende frames hergebruiken de laatste detectie. Lager = versere
posities tijdens snelle actie, hoger = goedkoper. `3` (ongeveer elke
0,1s bij 30fps) is een goede standaard; ga alleen naar 10-15 als je de
rekenkracht echt nodig hebt.

**Ball anchor range** (radialen, `player_anchor_max_rad`) - een poort
binnen de bal-*tracker* zelf (`crates/reco-autocam/src/trackers/ball.rs`),
vóór alles anders in dit document. Een ruwe baldetectie wordt alleen
geaccepteerd als hij binnen deze afstand van minstens één gevolgde
speler ligt; verdere detecties worden als vermoedelijke valse
positieven (een balvormig object in het publiek of de achtergrond)
weggegooid voordat de panner ze ooit ziet. `0,20 rad` (~11°) is de
standaard. Dit gebeurt *vóór* Ball reach (hieronder), dus een echte,
geïsoleerde bal - precies het uitbraak/hoek-scenario waar Ball reach en
FOV Wide voor bedoeld zijn - kan hier al stilletjes worden weggegooid,
waardoor die twee instellingen lijken alsof ze niet werken. Geverifieerd
via inspectie van de ruwe detectielogs (niet geraden): het model
detecteerde een uitgebroken bal correct met 0,97 zekerheid terwijl de
tracker aan het coasten was / de bal kwijtraakte, omdat de detectie
buiten deze poort viel. Verbreed 'm (0,3-0,5+) als de panner een bal die
echt ver van de groep is nooit lijkt op te pikken; houd 'm smal als het
model valse positieven geeft op rommel op de achtergrond.

**Style preset** - een eenmalige actie die elke slider hieronder
overschrijft (framing, cluster mode, lock-pitch, cluster bandwidth,
dead-zone, ball weight, ball reach, FOV) met een afgestemde set waarden.
Je kunt daarna nog elke slider los aanpassen; een andere preset kiezen
overschrijft alles opnieuw. Zie [Presets](#presets) hieronder voor de
exacte waarden per preset.

**Framing** - los van de preset-knoppen is dit de eigenlijke
algoritme-schakelaar: `action` richt zich op de (optioneel naar
zekerheid gewogen) groep spelers met edge-push/pitch-bias/bal-menging en
dynamische zoom. `frame_all` richt zich op het simpele geometrische
midden van de bounding box van alle spelers samen - geen trimming, geen
weging, geen bal-aantrekking - de modus "heel het team in beeld"
(trainingsbeelden, frisbee, etc.).

**Pitch - Lock (alleen horizontaal)** - uit (standaard) laat de tilt van
de camera ook de verticale positie van de actie volgen. Aan houdt de
tilt vast en pant alleen in yaw (horizontaal).

**Lookahead (soepelheid)** - buffert N seconden aan toekomstige frames
zodat de panner kan middelen over verleden+toekomst en de actie iets
kan vooruitlopen in plaats van per frame te reageren. `0,5s` is een
veilige middenwaarde; hoger is soepeler maar kost meer VRAM (de
risico-gekleurde slider in het exportdialoog waarschuwt als het niet
past bij de geladen bronresolutie).

## Geavanceerde panner

**Cluster mode** - `density` (standaard): centreert op de dichtste
concentratie spelers (de meeste buren binnen `cluster_bandwidth_rad`)
en houdt die groep vast - een verre kluit spelers kan de blik niet weg
trekken van de echte actie. `trimmed_mean`: middelt *alle* spelers,
waarbij de verste uitschieters worden getrimd - eenvoudiger, maar een
verre groep kan het gemiddelde nog beïnvloeden voordat het trimmen
ingrijpt.

**Ball weight** `[0-1]` - mengverhouding van de bal ten opzichte van de
spelersgroep (alleen bij Action-framing). De effectieve trekkracht per
frame is `ball_weight × ball_presence` (een waarde die oploopt zolang de
bal dicht bij de groep is en afneemt zodra dat niet meer zo is), dus het
trekt de camera alleen terwijl de bal daadwerkelijk aanwezig en dichtbij
is. Wordt geforceerd naar `1,0` in Ball-tracking-modus.

**Ball reach** (radialen, `ball_max_dist_from_cluster`) - hoe ver de bal
van het middelpunt van de spelersgroep mag afwijken en toch nog in de
aim wordt meegenomen (alleen bij Action-framing). Voorbij deze straal
wordt de bal behandeld als niet-relevant - een losse detectie of het
verre doel - en genegeerd, zodat hij de camera niet van het spel af kan
trekken. `0,5 rad` is de standaard. Verhoog dit als de camera een echte,
geïsoleerde bal niet volgt (bijv. de hoek in, bij een lange bal of een
uitbraak); verlaag het om de camera bij de groep te houden, ook als de
bal even loskomt.

**Cluster bandwidth** (radialen) - de straal van de buurt die `density`
gebruikt om te bepalen welke spelers bij "de" groep horen. Breder trekt
een losser/meer verspreid opgestelde formatie in één groep; smaller
isoleert een compacte kern (maar kan de groep sneller kwijtraken bij
open spel). Heeft geen effect onder `trimmed_mean`.

**Dead-zone** (radialen) - de camera blijft stilstaan zolang het
doelwit binnen deze straal van het huidige aim-punt blijft; grotere
afwijkingen worden geleidelijk gevolgd in plaats van direct
overgenomen. Verwijdert micro-gewiebel bij bijna-stilstaand spel.
Groter = rustiger maar iets trager reagerend (lookahead maakt dit
betaalbaar); kleiner = reactiever maar gevoeliger voor
micro-aanpassingen.

**Field of view - Tight / Default / Wide** (graden) - **geen** drie
vaste zoomniveaus. Het zijn de *grenzen* van een continu variërende
zoom die elk frame opnieuw wordt berekend op basis van de spreiding van
de spelersgroep plus afstands-/rand-/snelheidsbiases (Action-framing) of
de omvang van de bounding box (FrameAll). Tight = hoe ver de panner mag
inzoomen op een compacte groep; Wide = hoe ver uit bij verspreid spel;
Default is enkel de startwaarde voordat er spelers gedetecteerd zijn.
Als een specifieke situatie verder in-/uitzoomt dan gewenst, pas dan
deze grenzen aan - het is geen "voorkeurs"-middenwaarde.

**Wide is ook het plafond voor de bal-reach-verbreding** - `target_fov`
(dezelfde functie die de grenzen hierboven berekent) verbreedt het beeld
al om een gevolgde bal in beeld te houden (`ball_offset` +
`ball_frame_margin_deg`, verdubbeld), maar die berekende waarde wordt
daarna geclamped op `fov_wide`. Bij een echte uitbraak kan de benodigde
breedte het plafond van de `action`/`broadcast` presets (48-58°)
overschrijden, waardoor het beeld nooit breed genoeg wordt om zowel de
balbezitter als de hoofdgroep vast te houden - ook niet met **Ball
reach** opgetrokken. Zie de hoek-uitbraak-bullet hieronder.

**Dead-zone versus beeldmarge - twee verschillende dingen, makkelijk te
verwarren.** Dead-zone is een *reactiedrempel*: hoeveel het doelwit moet
bewegen voordat de camera überhaupt beweegt (zie hierboven). Dit heeft
niets te maken met hoe dicht een speler/bal bij de rand van het beeld
mag komen. Dat wordt geregeld door `ball_frame_margin_deg`
(Action-framing, verbreedt de FOV om deze marge rond de bal aan te
houden) en `frame_all_margin_deg` (FrameAll, marge rond de volledige
bounding box van alle spelers zodat niemand aan de rand van het beeld
wordt afgesneden) - zie [Extra parameters](#extra-parameters-nog-niet-beschikbaar-in-de-gui);
`ball_frame_margin_deg` heeft vandaag geen GUI/CLI-instelling.

## Presets

| Veld | default() | `broadcast` | `action` | `frame_all` |
|---|---|---|---|---|
| framing | Action | Action | Action | **FrameAll** |
| confidence_weighted | true | true | true | **false** |
| dead_zone_rad | 0,20 | 0,20 | **0,12** | 0,20 |
| fov_tight / default / wide | 22 / 40 / 58 | 22 / 40 / 58 | **20 / 34 / 48** | 22 / 40 / **70** |
| ball_weight | 0,50 | **0,20** | **0,35** | **0,0** |
| ball_max_dist_from_cluster | 0,50 | 0,50 | 0,50 | 0,50 |
| edge_push | 0,15 | 0,15 | **0,20** | 0,15 |
| lookahead_reactivity | 2,5 | 2,5 | **3,0** | 2,5 |
| frame_all_margin_deg | 8,0 | 8,0 | 8,0 | **10,0** |

`broadcast` is de gevalideerde, rustige standaard (alleen `ball_weight`
verlaagd t.o.v. de basisstandaard). `action` is strakker en reactiever
over de hele linie - kleinere dead-zone, smaller/strakker FOV-bereik,
sterkere bal-aantrekking, meer edge-push. `frame_all` schakelt het
framing-algoritme volledig om.

Bron: `FieldPannerConfig::{broadcast, action, frame_all}` in
`crates/reco-autocam/src/panners/field.rs`.

## Praktische afstelling voor voetbal

- **Rustig, weinig wisselingen (broadcast-stijl)**: laat de `broadcast`
  preset staan, of verlaag `ball_weight` nog verder (0,10-0,15) voor
  nog minder bal-achtervolging.
- **Snellere, energiekere follow bij counters**: schakel volledig over
  op de `action` preset in plaats van losse sliders handmatig af te
  stemmen - het is een afgestemde set, geen losse knop.
- **Groep "verliest zichzelf" telkens tijdens verspreid middenveldspel**:
  verhoog `cluster bandwidth` (0,35-0,4 rad).
  Verhoog [`min_cluster`](#extra-parameters-nog-niet-beschikbaar-in-de-gui)
  als 2 spelers te snel al een groep vormen (nu niet in de GUI
  beschikbaar).
- **Beeld voelt onrustig/schokkerig bij statisch spel**: verhoog
  `dead-zone`, of verhoog `lookahead` voor meer vooruitlopende
  soepelheid.
- **Camera zoomt verder in/uit dan gewenst in een specifieke situatie**:
  pas de FOV Tight/Wide-grenzen direct aan - het zijn grenzen, geen
  doelwaarden, dus verbreden/versmallen ervan verandert het *bereik*
  waarbinnen de dynamische zoom mag bewegen.
- **Camera volgt de bal niet de hoek in / bij een uitbraak**: check drie
  instellingen samen, in deze volgorde (elke poort filtert voor de
  volgende):
  1. **Ball anchor range** - als de tracker de verre detectie al
     nooit accepteert, maakt de rest niets uit. Verbreed 'm eerst
     (0,3-0,5+) en bevestig via `--events`/de events-JSONL dat de
     `state` van de bal op `Tracking` staat in plaats van te blijven
     hangen op `Coasting`/`Lost` tijdens de uitbraak.
  2. verhoog `ball reach` (`ball_max_dist_from_cluster`) boven de
     standaard van 0,5 rad - **en**
  3. verhoog ook FOV Wide (probeer 65-70°). Ball reach alleen
  ontgrendelt de aim-pull richting de bal; het beeld heeft daarnaast een
  hoge genoeg `fov_wide` nodig zodat de bal-verbredingsberekening
  (zie hierboven) zijn doelwaarde ook echt kan bereiken in plaats van
  geclamped te worden. Geverifieerd via een gecontroleerde A/B-render op
  dezelfde clip/moment, alle andere instellingen gelijk gehouden: bij
  `fov_wide: 48°` (de standaard van de `action` preset) passen de
  balbezitter en de hoofdgroep niet allebei in beeld; bij `70°` wel. Dit
  is een bewuste ontwerpkeuze van Action-framing - een echte, geïsoleerde
  bal ver van de groep wordt standaard genegeerd zodat een losse detectie
  of het verre doel de camera niet kan wegtrekken; beide knoppen verhogen
  betekent meer vertrouwen in de detector, met het risico dat af en toe
  een valse positieve wordt gevolgd. Als bal-acties belangrijker zijn dan
  bij de groep blijven, is overschakelen naar **Tracking mode -> ball**
  voor die wedstrijd vaak een betere keuze.

## Extra parameters (nog niet beschikbaar in de GUI)

Een aantal velden van `FieldPannerConfig` heeft vandaag geen instelling
in het Export-dialoogvenster en is alleen te wijzigen via een
configuratiebestand / CLI-vlag die rechtstreeks `reco-autocam`
aanspreekt: `min_cluster`, `edge_push`, `fov_alpha`,
`pitch_near`/`pitch_far`/`distance_bias_max`, `edge_bias_max`,
`cluster_alpha`, `max_velocity_rad_per_sec`, `velocity_alpha`,
`pitch_bias`, `ball_presence_decay`/`ball_presence_attack`,
`velocity_fov_bias_max`, `ball_frame_margin_deg`,
`lead_gain`/`lead_alpha`,
`keep_fraction` (alleen trimmed-mean). Zie de doc-comments per veld in
`crates/reco-autocam/src/panners/field.rs` voor wat elk precies doet.
