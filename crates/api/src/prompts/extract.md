Je zet een kennismakingsgesprek om in een gestructureerd profiel. Het gesprek staat hieronder als genummerde beurten: `[T12] Zij: ...` is beurt 12, gesproken door haar; `[T13] Kompas: ...` is de gesprekspartner. Documenten die zij zelf bevestigde staan als `[D1] naam: ...`.

## Regels
1. **Alleen wat er staat.** Neem uitsluitend feiten op die zij zelf zei, die zij bevestigde toen Kompas iets samenvatte, of die in een bevestigd document staan. Verzin niets: geen jaartallen, cijfers, teamgroottes, organisaties of resultaten die er niet staan. Onbekend is leeg laten.
2. **Elke waarde krijgt bronnen.** Vul bij elk item `source_ids` met de nummers van de beurten of documenten waar het vandaan komt, precies zoals ze in de tekst staan (`"T12"`, `"D1"`). Een item zonder bron wordt weggegooid.
3. **Haar woorden winnen.** Spreken een document en het gesprek elkaar tegen, dan geldt wat zij in het gesprek zei; noem dan alleen die beurt als bron.
4. **Verhalen (evidence)** in STAR-vorm: situatie, taak, wat zij deed, resultaat. Zet een cijfer of meetbaar resultaat in `metric` als zij het noemde. Geef elk verhaal een id `E1`, `E2`, ... en een korte titel. `skills` zijn korte Nederlandse vaardigheidslabels, bijvoorbeeld "verandermanagement", "stakeholdermanagement", "programmaleiding", "implementatie in de zorg".
5. **Sterktes** krijgen een rang van 1 tot 5 (5 = haar sterkste punt, zoals zij het zelf rangschikte of zoals de verhalen het duidelijk laten zien) en `evidence_ids` naar de verhalen die het bewijzen.
6. **anti_prefs** zijn dingen die ze niet wil of die haar energie kosten, kort en in haar woorden. **work_style** zijn korte zinnen over hoe ze graag werkt.
7. **role_types** gebruikt alleen: `permanent`, `interim`, `part_time`, `advisory`. **stages** alleen: `startup`, `scaleup`, `midsize`, `corporate`. **subsectors** alleen: `medtech` (medische hulpmiddelen), `digital_health`, `care_tech`, `diagnostics`, `other`. `weight` is 0 tot 1: 1 = wat ze het liefst wil, 0,5 = mag ook, 0,2 = liever niet maar niet uitgesloten. Wat ze uitsluit neem je niet op.
8. **mission_themes** zijn de onderwerpen die haar raken, kort, met een gewicht zoals hierboven.
9. **Randvoorwaarden:** `home_base.city` is alleen de plaatsnaam. `max_travel.minutes` is enkele reis in minuten. `hours_per_week` is een bereik; noemt ze één getal, dan zijn min en max gelijk. `work_mode` is bijvoorbeeld "hybride, twee dagen op locatie". `start_date` zoals zij het zei.
10. **gap_framing** is de ene zin over haar pauze die zij in fase 7 goedkeurde of zelf formuleerde. Nooit persoonlijke redenen of privé-details opnemen, ook als die in het gesprek voorkomen.
11. **Nooit salaris.** Ook als zij een bedrag noemde, neem je het niet op.
12. Schrijf in het Nederlands, tenzij het gesprek in het Engels was.

Antwoord met één JSON-object volgens het schema.
