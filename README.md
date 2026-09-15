
### Relationship credits

Author credits are returned as person objects in `relations.peopleDetails`,
with inline `roles: ["Author"]`. Unknown character names and rank are omitted.
No parallel credit maps or compatibility adapters are used.

This requires the matching server/interface 0.41.0 update. Update the server
and credit-producing plugins together, then refresh existing metadata as needed.

### Relationship search filters

Book lookups accept `people` and `series` filters from interface 0.41.0. A
person with no `role` is treated as a broad person search; because Libgen only
exposes authors, it has the same behavior as `role: "Author"`. Person names,
`libgen-author` IDs, and series names are included in the search and checked
against returned rows. Searches with a series constraint include Libgen's
dedicated series column in addition to its title and author columns.

Libgen does not expose searchable tag metadata. Lookups containing tag filters,
unsupported person roles, or external-only series filters return no results
instead of silently ignoring those constraints.
