---
name: goplaces-finite
description: Look up places with Google Places API (New) using text search and place details. Use for high-quality business/place lookup, maps links, ratings, and hours.
version: 1.0.0
author: local
license: MIT
metadata:
  tags: [Google, Places, Maps, business lookup, local search]
  related_skills: [find-nearby-finite]
  homepage: https://developers.google.com/maps/documentation/places/web-service/text-search
---

# GoPlaces

Use Google Places API (New) for high-quality place and business lookup.

This skill is different from `find-nearby-finite`:
- `find-nearby-finite` is the no-key fallback
- `goplaces-finite` uses the official Google Places API for stronger commercial/business data and Google Maps links

## Required Credential

Your computer holds no keys: `GOOGLE_PLACES_API_KEY` holds the operator's
key's placeholder (it names you), the helper sends it in `X-Goog-Api-Key`
(or a client may send it as `?key=`) to `places.googleapis.com`, and the
computer swaps in the real key, each call metered to your owner. Unset,
this deployment does not offer the `google-places` key: use
`find-nearby-finite` instead and say so.

## Helper Script

Search:

```bash
python ${SKILL_DIR}/scripts/google_places.py search --query "best coffee shops in austin"
```

Place details:

```bash
python ${SKILL_DIR}/scripts/google_places.py details --place-id ChIJ...
```

## Common Patterns

### Text Search

```bash
python ${SKILL_DIR}/scripts/google_places.py search   --query "best sushi near downtown Austin"   --max-results 5
```

### Location-Biased Search

```bash
python ${SKILL_DIR}/scripts/google_places.py search   --query "pharmacy"   --lat 30.2672 --lon -97.7431 --radius-meters 2500
```

### Full JSON Output

```bash
python ${SKILL_DIR}/scripts/google_places.py search   --query "coworking spaces in chicago"   --json
```

### Place Details

```bash
python ${SKILL_DIR}/scripts/google_places.py details --place-id ChIJN1t_tDeuEmsRUsoyG83frY4
```

## Guidance

- Use `goplaces-finite` when the user wants high-confidence business/place lookup.
- Use `find-nearby-finite` when you want a free/no-key fallback or rough nearby discovery.
- Prefer `search` first, then `details` for the chosen place.
- Include `googleMapsUri` when handing a place back to the user.

## Notes

- This skill uses Google Places API (New), which requires a field mask.
- Live billable requests were not smoke-tested here on purpose.
- The helper script is installed and locally verified with `--help` only.

## Sources

- Text Search (New): https://developers.google.com/maps/documentation/places/web-service/text-search
- Place Details (New): https://developers.google.com/maps/documentation/places/web-service/place-details
- Choose fields: https://developers.google.com/maps/documentation/places/web-service/choose-fields
