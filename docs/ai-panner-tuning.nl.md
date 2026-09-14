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

Elke schuifregelaar in deze sectie slaat bovendien meteen op als jouw
app-brede "laatst gebruikte" standaard, zodra je hem wijzigt - geen
Save calibration nodig. Bij het herstarten van `reco-gui` worden deze
waarden hersteld voordat er video of calibratie geladen is; een
calibratie met eigen opgeslagen `autocam_defaults` overschrijft ze
daarna alsnog, zelfde prioriteit als voorheen. Zie
[`GuiSettings::autocam_defaults`](../crates/reco-gui/src/settings.rs).

Model: `yolo26n_v2` is de productiecheckpoint op moment van schrijven,
maar `yolo26s_v3` (ronde 3, ONNX-geëxporteerd) scoorde dramatisch beter
in een echte in-app test op dezelfde clip - ruwe baldetecties van 19,7%
naar 48,7% van de frames. Nog niet gepromoveerd tot "de" standaard, zie
`docs/YOLO26_Training.md`. Zelfs `yolo26s_v3` mist de bal echter voor langere
tijd op deze clip - frames 720-898 (de laatste ~6sec van het
gevalideerde 100-130sec 03 OJC-venster) hebben helemaal geen ruwe
baldetecties, bevestigd 2026-08-12 over alle drie de `ball_weight`
A/B-renders heen. Een reëel model-recall-gat, niet iets dat een
panner-instelling oplost - zie de trainingsnotities voor bekende
lastige gevallen.

```
Tracking mode:                      field
Detect every N frames:              3
Ball anchor range:                  0,3-0,5 rad     (standaard 0,20)
Ball coast time:                    2,5s            (standaard 0,67s) - zie notitie hieronder
Style preset:                       action

Framing:                            action
Pitch - Lock (alleen horizontaal):  uit
Lookahead:                          0,5s
Reduce lookahead memory (8-bit):    aan bij een 10-bit bron (zie notitie hieronder), anders uit

Advanced panner
----------------
Cluster mode:                       trimmed_mean
Ball weight:                        0,5             (standaard van de action preset is 0,35)
Dead-zone:                          0,05-0,08 rad
Cluster bandwidth:                  0,3 rad         (preset-standaard, niet apart getuned)
Ball reach:                         1,0 rad         (standaard 0,5)
FOV Tight:                          20deg           (preset-standaard, niet apart getuned)
FOV Default:                        34deg           (preset-standaard, niet apart getuned)
FOV Wide:                           65-70deg        (standaard van de action preset is 48deg)
Zoom smoothing (fov_alpha):         0,05-0,08       (standaard 0,01)
Aim smoothing (cluster_alpha):      0,05-0,08       (standaard 0,012)
Reactivity (lookahead):             1,5             (standaard 2,5, action preset 3,0) - zie notitie hieronder
```

Waarom elk van deze, kort: **Cluster mode -> trimmed_mean** fixt de
freeze van meerdere seconden bij balloze fases. **Dead-zone** is nodig
samen met `trimmed_mean`, samen getest. **Ball weight 0,5** (opgetrokken
van de action-preset-standaard 0,35, gevalideerd 2026-08-12 tegen echte
100-130sec 03 OJC-beelden) - bij 0,35 leunt de aim-blend nog te zwaar op
het middelpunt van de spelerscluster zodra de bal daar verticaal van
losbreekt (bv. richting de dichtstbijzijnde zijlijn terwijl de spelers
hoger op het veld blijven): het *berekende doel* zelf komt dan nooit
dicht genoeg bij de bal, die vervolgens uit beeld drijft ongeacht hoe
snel de smoothing reageert - geen smoothing-probleem, een
blend-weight-probleem. 0,5 houdt de bal in beeld voor de hele geteste
uitbraak; 0,6 werkt ook, zonder verder voordeel. Kosten: camerabeweging
per frame stijgt mee (+33% gemiddeld, +32% p95 verschil per frame bij
0,5 t.o.v. 0,35 in dezelfde test) - een reële wobble-afweging, maar bij
lange na niet zo erg als `1,0`, wat de daadwerkelijk zichtbare wobble
gaf, ongeacht modelkwaliteit. **Ball reach** laat de panner
richting een echt geïsoleerde bal trekken i.p.v. 'm te negeren. **FOV
Wide** - zonder dit op te trekken clamped de bal-reach-verbredingslogica
voordat het beeld echt breed genoeg kan worden. **Zoom/Aim smoothing** -
zelfs met Ball reach en FOV Wide opgetrokken zijn de *standaard*
smoothing-snelheden vaak te traag om de bredere/verplaatste doelwaarde
daadwerkelijk te bereiken voordat een korte uitbraak alweer voorbij is
(zie hieronder) - trek deze op als de camera een snelle balactie
halverwege lijkt "op te geven". **Reactivity (lookahead) 1,5** (omlaag
van de standaard 3,0 van de action-preset, gevalideerd 2026-09-03 op
een echte XFT-UHTF-wedstrijd) fixt een ander symptoom: de camera
"sprint" zichtbaar naar topsnelheid in plaats van er rustig naartoe te
komen bij een grote, plotselinge verplaatsing van het doel (bv. de bal
die ~10m verschuift). Deze vermenigvuldiger verhoogt zowel de
snelheidslimiet als de optrek-snelheid van de basis-chase zolang
lookahead actief is, dus een hoge waarde schiet flink door voordat de
gecentreerde smoother de resulterende schok kan wegwerken. Verlagen
naar 1,5 verminderde de piek per-frame-sprong en de piek
halve-seconde-schommeling ~45% in een echte CLI-A/B-render, terwijl
bal-tracking-aanwezigheid en de *typische* (niet-piek) soepelheid
ongewijzigd bleven - die asymmetrie (pieken omlaag, gemiddelde
ongewijzigd) is precies het verwachte effect van deze instelling, geen
toeval.

**Reduce lookahead memory (8-bit) - momenteel verplicht bij 10-bit
bronnen, niet alleen een VRAM-noodgreep.** Staat dit uit, dan crasht een
10-bit bron (bv. DJI Action 4 HEVC, `P010`) via het standaard zero-copy
decodepad reproduceerbaar: wgpu's Dx12-backend weigert de plane-copy van
de lookahead-pool met `Source format (P010) and destination format
(R16Unorm) are not copy-compatible`. Bevestigd 2026-08-12 bij het
stitchen van een echte clip met `--lookahead 0.5` zonder
`--lookahead-reduced-bit-depth` - zie de doc-comment bij
[`VramPool::copy_from_d3d11`](../crates/reco-core/src/session/vram_pool.rs)
voor de volledige repro en root-cause-notities. Nog niet gefixt - laat
dit tot die tijd **aan** staan bij elke 10-bit bron met lookahead
ingeschakeld (of gebruik `--no-zero-copy`, ten koste van echte
decodesnelheid, of `--lookahead 0` om lookahead helemaal uit te
schakelen). 8-bit bronnen ondervinden hier geen last van.

De drie bal-gerelateerde *gates* filteren voor elkaar, in deze
volgorde: **Ball anchor range -> Ball reach -> FOV Wide**. Ball anchor
range bepaalt of de *tracker* een verre detectie uberhaupt accepteert;
Ball reach bepaalt of de *panner* 'm de aim mag laten trekken; FOV Wide
bepaalt of het *beeld* daadwerkelijk breed genoeg kan worden om het te
tonen. Alleen één van de drie verhogen lost een gemiste uitbraak niet
volledig op - zie de bullet "Camera volgt de bal niet de hoek in"
verderop voor de volledige onderbouwing en hoe elk is geverifieerd (niet
alleen aanbevolen op basis van een gok).

**Ball weight is geen gate maar bepaalt hoe hard de aim daadwerkelijk
achter de bal aan gaat zodra die alle drie doorstaat** - gevalideerd
2026-08-12 dat zelfs met elke gate hierboven open, `ball_weight 0,35` op
zichzelf nog niet genoeg is: als de bal ver genoeg *verticaal* van de
spelerscluster losbreekt (een uitbraak richting de zijlijn, niet alleen
horizontaal), houdt de blend het berekende aim-doel te dicht bij de
cluster en drijft de bal alsnog uit beeld. Zie de "Ball weight"-regel
hierboven en de checklist "Camera volgt de bal niet de hoek in" verderop
(nu 5 instellingen, niet 4).

**Elke instellingentabel hierboven wordt ook weggeschreven naar de
events-JSONL.** Als "Record pipeline events" (`--events` op de CLI) aan
staat *en* AI tracking is ingeschakeld, is de allereerste regel van het
uitvoerbestand een `{"kind":"run_config", ...}` record met elk veld uit
bovenstaande tabellen - zodat een trace-bestand zelfbeschrijvend is
zonder terug te hoeven zoeken naar het export-commando of de
GUI-instellingen. Zie
[`PipelineEvent::RunConfig`](../crates/reco-core/src/detect/pipeline_event.rs).

## Instellingen op hoofdniveau

**Tracking mode** - `field` (standaard): volgt de groep spelers + de bal
samen; valt terug op alleen-bal als het model geen spelersklasse heeft.
`ball`: volgt alleen de bal (hogere zekerheidsdrempel, strengere
max-sprong-grens). `sweep`: geen AI - een vaste sinusvormige links-rechts
pan, alleen bruikbaar als debug/basislijn-modus.

**Detect every N frames** - hoe vaak de detector daadwerkelijk draait;
tussenliggende frames hergebruiken de laatste detectie. Lager = versere
posities tijdens snelle actie, hoger = goedkoper. `3` (ongeveer elke
0,1s bij 30fps) is een goede standaard; ga pas hoger als je de
rekenkracht echt nodig hebt - in de praktijk getest op 2026-08-29 op
een live export: van 3 naar 10 verdrievoudigde bijna het aantal fps
(19 -> 52fps), veruit de grootste performance-hendel in de hele panner.
De impact op de trackingkwaliteit van juist die wijziging is niet apart
opnieuw gevalideerd - beschouw de fps-winst als bevestigd en de impact
op trackingkwaliteit als onbekend, niet als nul.

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

**Ball anchor range - near/far-ramp (alleen CLI, nog geen GUI-slider)**
- één vlakke straal dekt een echte, gekoppelde bal dichtbij een naar
beneden gekantelde rig onvoldoende: dezelfde afstand "teamgenoot vlak
bij de bal" in het echt komt dichtbij de camera (steile kijkhoek) neer
op een veel *groter* gat in panorama-ruimte (yaw, pitch) dan ver het
veld in (ondiepe hoek, dicht bij de horizon). Waargenomen op echte
XFT-UHTF-beelden: een bal met 90% zekerheid slechts 1-2° buiten een
vlakke poort van 17° rond het midden van het veld, naast een echte bal
~25° van zijn dichtstbijzijnde teamgenoot dichtbij de camera.
`--player-anchor-rad-near` zet een aparte straal die geldt bij een
wereld-pitch <= -0,05rad (dichtbij de camera), die lineair oploopt naar
de waarde van `--player-anchor-rad` (het "verre" uiteinde, bij pitch >=
0,20rad) ertussenin; heeft geen effect tenzij `--player-anchor-rad` ook
is ingesteld. **Getest en afgewezen bij 30°** (2026-09-03): liet een
stilstaand wit veldmarkering-schijfje op de zijlijn 65+ seconden
achtereen als valse "bal" door. Het mechanisme zelf is degelijke
infrastructuur - alleen deze specifieke brede waarde wordt afgewezen.
Verhoog dit niet zonder nieuw bewijs; een kleinere near-waarde opnieuw
valideren tegen hetzelfde stilstaande-marker-geval is de openstaande
volgende stap. Zie
[`BallTracker::with_player_anchor_rad_near_far`](../crates/reco-autocam/src/trackers/ball.rs).

**Ball coast time** (seconden, `ball_coast_secs`) - hoe lang een bal die
al gevolgd wordt zijn laatst bekende positie vasthoudt nadat detecties
stoppen, voordat de track als kwijt wordt beschouwd. Gemeld door de
gebruiker op 2026-08-14: een bal die buiten de gekalibreerde
veld-ROI-polygon rolt of geschoten wordt (zie
[ROI-filtering](../crates/reco-autocam/src/roi_filter.rs)) ziet er voor
de tracker precies zo uit als "geen detectie dit frame" - het
ROI-filter gooit 'm weg voordat de tracker 'm ooit ziet, net als wanneer
het model 'm gewoon gemist had. Zonder deze instelling gaat de track
bijna direct van `Coasting` naar `Lost`, waardoor de camera een speler
die over de lijn stapt om de bal te halen nooit volgt. Dit is geen
nieuwe trackinglogica: [`BallTracker`](../crates/reco-autocam/src/trackers/ball.rs)
gebruikte al een vast coast-budget
([`DEFAULT_COAST_FRAMES = 20`](../crates/reco-autocam/src/trackers/ball.rs),
via de herbruikbare [`Coaster`](../crates/reco-autocam/src/trackers/filters/coaster.rs)
frame-aftel-helper) om elke korte detectie-onderbreking te overbruggen,
ongeacht de oorzaak - het was alleen nog niet instelbaar of zichtbaar in
de UI. Optrekken verlengt alleen hoelang een *al gevolgde* bal wordt
vastgehouden; een bal die nooit gevolgd werd (bv. kinderen die naast het
veld met een bal aan het opwarmen zijn) start deze aftelling nooit, dus
optrekken maakt de panner niet gevoeliger voor dat geval. Standaard is
`0,67s` (het bestaande budget van 20 frames bij 30fps). Functioneel
end-to-end geverifieerd op 2026-08-14 (de waarde komt correct terecht in
het events-JSONL run_config-record via `--ball-coast-secs`); door de
gebruiker bevestigd op echte beelden op 2026-08-15 dat `2,5s` "aardig
werkt" voor het ROI-overgangsscenario - een informele bevestiging op de
eigen wedstrijdbeelden van de gebruiker, geen gecontroleerde A/B-render
zoals de andere instellingen op deze pagina, maar wel een echt resultaat
in plaats van een gok. Begin bij `2,5s`; bevestig via `--events` dat de
`state` van de bal `Coasting` blijft in plaats van naar `Lost` te
springen tijdens de overgang als je verder wilt tunen.

**Detection confidence** (`[0,1]`, `confidence_threshold`) - een ruwe
detectie van elke klasse (persoon, bal, scheidsrechter) onder deze score
bereikt de tracker helemaal nooit - het is de bodemgrens die de detector
zelf toepast voordat iets stroomafwaarts (Ball anchor range, de panner,
...) een kandidaat ooit ziet. Standaard `0,10`. Toegevoegd 2026-08-29
nadat het per-export logbestand (`.log` naast de output, zie
`reco_io::export_log`) een echt patroon zichtbaar maakte in de timeline:
tientallen `BallTracker: acquired ... conf=0,10-0,20`-momenten in één
export, elk een verse, grotendeels ongesmoothte ruwe bal-positie die
rechtstreeks in de aim-blend terechtkomt (`ball_weight`) - een
aannemelijke, andere bron van zichtbare camera-wobble dan de
smoothing-snelheid-oorzaken hierboven. Concurrentie-referentie: een
lokaal geïnstalleerd concurrerend product heeft in zijn eigen runtime-log
`detectorConfidenceThreshold=0.55` staan, 5,5x deze standaard - geen
bewijs dat 0,55 hier ook klopt (ander model/andere pipeline), maar wel
een echt datapunt als je dit optrekt. **Vervangt verborgen, hardcoded
gedrag**: Ball-tracking-modus dwong dit voorheen stilletjes af naar
`0,25`, ongeacht wat dan ook - dat gebeurt niet meer, en de oude
Field/Sweep-waarde `0,10` is nu gewoon de standaard van deze ene slider
voor elke modus. Als je op de oude, impliciete 0,25-bodemgrens van
Ball-modus vertrouwde, zet 'm nu expliciet. Nog niet A/B-gevalideerd op
echte beelden - trek 'm een stapje op (bv. `0,2-0,3`) en check de
`BallTracker: acquired`-regels in het exportlog op minder momenten met
zeer lage confidence, en let op of een echt zwakke bal daardoor juist
gemist wordt - dat is de afweging.

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

**Zoom smoothing / Aim smoothing** (`fov_alpha` / `cluster_alpha`) - hoe
snel de *gesmoothde* zoom en aim van de panner elk frame hun berekende
doelwaarde bijtrekken, als een exponentieel-voortschrijdend-gemiddelde-
snelheid (geen vertraging of plafond). Dit is een compleet losse knop
van Lookahead: Lookahead bepaalt hoeveel toekomst/verleden wordt
meegemiddeld in de doelwaarde zelf; deze twee bepalen hoe snel de
getoonde waarde die doelwaarde achterna gaat zodra die berekend is. De
standaardwaarden (`fov_alpha: 0,01`, `cluster_alpha: 0,012`) hebben een
tijdconstante van ongeveer **3 seconden bij 30fps** - bevestigd via een
echte trace: op een hoek-uitbraak-clip klom de FOV maar van 38,7° naar
39,9° (doelwaarde was al voorbij 65°+) over de ~20 frames dat de bal
volgbaar bleef, en de aim-pitch bewoog nauwelijks terwijl de pitch van
de bal in datzelfde venster 0,24 rad verschoof. De bal was het beeld al
uit voordat de smoothing had bijgetrokken. Trek beide op als de camera
een snelle uitbraak halverwege lijkt "op te geven" ondanks dat Ball
reach/FOV Wide al hoog genoeg staan; te ver optrekken kan de wobble
terugbrengen die deze twee juist moesten voorkomen, want een sneller
reagerende camera jaagt ook gretiger een ruizige detectie achterna.

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
  soepelheid. Bevestigd 2026-09-03 op echte beelden: een groep kinderen
  die om een stilstaande/niet-gevolgde bal heen drentelde, bleef de
  camera onrustig laten meebewegen bij een lage dead-zone (0,048 rad
  op die calibratie); optrekken naar **0,15 rad** loste het op zonder
  waargenomen nadeel.
- **Beeld schommelt/hopt omdat de AI zo gefocust lijkt op de bal - elke
  kleine bal-beweging trekt het beeld mee** (gemeld 2026-08-29, **nog
  niet A/B-gevalideerd** - onderstaand advies is afgeleid uit de code,
  nog niet bevestigd op echte beelden): in tegenstelling tot de
  spelerscluster, die via `cluster_alpha` wordt gesmooth vóórdat hij het
  aim-doel bereikt, wordt de bal-positie zelf **rauw, zonder eigen
  smoothing** in het aim-doel gemengd - `doel = cluster * (1 - w) +
  laatste_bal_positie * w`, met `w = ball_weight * ball_presence` (zie
  [`FieldPanner::decide_with_lookahead`](../crates/reco-autocam/src/panners/field.rs)).
  Zodra de bal betrouwbaar gevolgd wordt (`ball_presence` bijna 1), komt
  dus de ruwe detectie-ruis van elke losse frame - of een echt
  stuiterende bal - direct door in waar de camera op richt, pas daarna
  afgezwakt door `dead-zone`/de snelheidslimiet. Twee dingen om te
  proberen, minst ingrijpend eerst:
  1. **Verhoog `dead-zone`** (bv. 0,05 -> 0,09-0,10 rad) - vangt kleine
     bal-jiggles op zonder dat een echte, grotere bal-beweging minder
     hard aan de aim mag trekken. Dit is precies waarvoor deze
     instelling al bestaat; lookahead staat al aan, dus de extra
     latency wordt al opgevangen.
  2. **Als dat niet genoeg is: verlaag `ball weight`** (bv. 0,5 -> 0,35)
     - vermindert direct hoeveel de rauwe, ongesmoothte bal-positie mag
     trekken. Afweging: 0,5 is precies de waarde die deze pagina verderop
     (hoek-uitbraak-checklist) aanraadt te *verhogen*, specifiek om te
     voorkomen dat de bal bij een verticale uitbraak uit beeld drijft -
     verlagen kan dat probleem terugbrengen.
  Een structureel schonere oplossing, als deze twee sliders het niet
  volledig oplossen zonder de bal-volgkracht op te geven, zou een eigen
  smoothing-snelheid voor de bal-bijdrage zijn (naast `cluster_alpha`) -
  nog niet gebouwd, want deze sliders zijn nog niet geprobeerd.
- **Camera zoomt verder in/uit dan gewenst in een specifieke situatie**:
  pas de FOV Tight/Wide-grenzen direct aan - het zijn grenzen, geen
  doelwaarden, dus verbreden/versmallen ervan verandert het *bereik*
  waarbinnen de dynamische zoom mag bewegen.
- **Camera volgt de bal niet de hoek in / bij een uitbraak**: check vijf
  instellingen samen, in deze volgorde (elke stap filtert of bepaalt de
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
  4. Als de bal ondanks alle drie bovenstaande correct opgetrokken nog
     steeds uit beeld valt, check dan **Zoom/Aim smoothing**
     (`fov_alpha`/`cluster_alpha`) - via een echte trace bevestigd dat de
     *standaard* smoothing-snelheden (~3s tijdconstante) vaak te traag
     zijn om de bredere/verplaatste doelwaarde te bereiken voordat een
     korte uitbraak alweer voorbij is, ook al werd de doelwaarde zelf
     wel correct berekend. Trek beide op naar ~0,05-0,08.
  5. Als de bal specifiek uit beeld valt zodra die *verticaal* van de
     spelerscluster losbreekt (richting de dichtstbijzijnde zijlijn, niet
     alleen zijwaarts), zelfs met alle vier bovenstaande al opgetrokken en
     smoothing al snel, trek dan **Ball weight** op (0,5, vanaf de
     `action`-preset-standaard van 0,35). Geverifieerd via een echte
     CLI-A/B-render (dezelfde clip, verder alles gelijk) dat het berekende
     aim-doel bij 0,35 in dit scenario nooit dicht genoeg bij de bal komt
     - geen poort- of smoothing-snelheidsprobleem, de blend leunt gewoon
     te zwaar op het middelpunt van de cluster. Afweging: verhoogt de
     algehele camerabeweging (+30-33% gemiddeld verschil per frame bij 0,5
     t.o.v. 0,35 in dezelfde test) - reëel, maar ruim onder de zichtbare
     wobble die `1,0` veroorzaakt.
- **Camera raakt de bal kwijt zodra die de veld-ROI-lijn oversteekt**
  (bv. een speler die over de zijlijn stapt om de bal te halen, of een
  inworp) **en volgt de terughaal-actie nooit**: dit heeft een andere
  oorzaak dan de hoek-uitbraak-checklist hierboven - de bal wordt
  weggefilterd door de ROI-polygon, niet afgewezen door een
  panner-poort. Trek **Ball coast time** op naar `2,5s` (zie hierboven)
  zodat een al gevolgde bal zijn laatste positie lang genoeg vasthoudt om
  de onderbreking te overbruggen. Helpt niet bij een bal die nooit
  gevolgd werd (bv. een tweede bal net naast het veld) - dat is het
  ROI-filter dat doet wat het hoort te doen, geen coast-time-probleem.

## Extra parameters (nog niet beschikbaar in de GUI)

Een aantal velden van `FieldPannerConfig` heeft vandaag geen instelling
in het Export-dialoogvenster en is alleen te wijzigen via een
configuratiebestand / CLI-vlag die rechtstreeks `reco-autocam`
aanspreekt: `min_cluster`, `edge_push`,
`pitch_near`/`pitch_far`/`distance_bias_max`, `edge_bias_max`,
`max_velocity_rad_per_sec`, `velocity_alpha`,
`pitch_bias`, `ball_presence_decay`/`ball_presence_attack`,
`velocity_fov_bias_max`, `ball_frame_margin_deg`,
`lead_gain`/`lead_alpha`,
`keep_fraction` (alleen trimmed-mean). Zie de doc-comments per veld in
`crates/reco-autocam/src/panners/field.rs` voor wat elk precies doet.
