# Mods replacing the same tables

Status: an idea, not started.

Lau's KNOWN-LIMITATIONS warns that a patch replacing a whole
`DBFilesClient/*.dbc` table can conflict with another patch that supplies
the same table. The indexer could record, for each release, the tables its
archives replace, and the launcher could list the other `Data/*.mpq` of the
client that ship the same table as "may conflict" with the mod. It would
only warn: those archives may be the player's own, which the launcher never
moves.
