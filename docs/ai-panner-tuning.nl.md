# AI-cameratracking & panner-afstelling

Naslagwerk voor de sectie **AI Tracking** in het Export-dialoogvenster
(`reco-gui`). Legt uit wat elke instelling precies doet en hoe je hem
afstemt voor voetbal. Gebaseerd op `crates/reco-autocam/src/panners/field.rs`
en `crates/reco-autocam/src/tracking_mode.rs` - niet geraden.

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

**Style preset** - een eenmalige actie die elke slider hieronder
overschrijft (framing, cluster mode, lock-pitch, cluster bandwidth,
dead-zone, ball weight, FOV) met een afgestemde set waarden. Je kunt
daarna nog elke slider los aanpassen; een andere preset kiezen
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

## Extra parameters (nog niet beschikbaar in de GUI)

Een aantal velden van `FieldPannerConfig` heeft vandaag geen instelling
in het Export-dialoogvenster en is alleen te wijzigen via een
configuratiebestand / CLI-vlag die rechtstreeks `reco-autocam`
aanspreekt: `min_cluster`, `edge_push`, `fov_alpha`,
`pitch_near`/`pitch_far`/`distance_bias_max`, `edge_bias_max`,
`cluster_alpha`, `max_velocity_rad_per_sec`, `velocity_alpha`,
`pitch_bias`, `ball_presence_decay`/`ball_presence_attack`,
`velocity_fov_bias_max`, `ball_frame_margin_deg`,
`ball_max_dist_from_cluster`, `lead_gain`/`lead_alpha`,
`keep_fraction` (alleen trimmed-mean). Zie de doc-comments per veld in
`crates/reco-autocam/src/panners/field.rs` voor wat elk precies doet.
