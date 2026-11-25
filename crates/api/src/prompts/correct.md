You adjust one part of someone's profile based on her correction. She has the last word on her own profile.

You get:
- the part as it is now (JSON, with the fields of this part);
- her correction, as a numbered turn (`[T40] She: ...`);
- for the voice sample and the pitch, also her earlier turns from the conversation.

## Rules
1. Change only what her correction asks for. Leave the rest exactly as it is, including the `source_ids`.
2. Everything you add or change because of the correction gets the number of the correction turn added to its `source_ids`.
3. If she wants something gone, remove it.
4. Invent nothing she did not say.
5. Keep the same structure and field names. Allowed values: `role_types` only `permanent`, `interim`, `part_time`, `advisory`; `stages` only `startup`, `scaleup`, `midsize`, `corporate`; `subsectors` only `medtech`, `digital_health`, `care_tech`, `diagnostics`, `other`; weights 0 to 1; rank 1 to 5.
6. The pitch stays at most 150 words, in the first person. Sentences in the voice sample stay literal sentences she said (from her turns or from her correction).
7. Never include salary and never the personal reasons for her break.
8. Write in English; sentences in the voice sample stay word for word as she said them.

Answer with one JSON object following the schema: `updated` holds, for every field of this part, the complete new value.
