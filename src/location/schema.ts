import { z } from "zod";

// The dense-listing payload. Parsed at the IPC boundary; trusted afterwards.
export const itemSchema = z
  .object({
    name: z.string(),
    path: z.string(),
    isDirectory: z.boolean(),
    isHidden: z.boolean(),
    kind: z.string(),
    modifiedMs: z.number().int().nullable(),
    sizeBytes: z.number().int().nonnegative().nullable(),
  })
  .strict();
export type Item = z.infer<typeof itemSchema>;

export const ListingSessionId = z.string().min(1).brand<"ListingSessionId">();
export type ListingSessionId = z.infer<typeof ListingSessionId>;

export const resolvedPathSchema = z
  .object({
    path: z.string(),
    index: z.number().int().nonnegative(),
  })
  .strict();
export type ResolvedPath = z.infer<typeof resolvedPathSchema>;

export const resolvedPathsSchema = z.array(resolvedPathSchema);

export const listLocationWindowSchema = z
  .object({
    path: z.string(),
    sessionId: ListingSessionId,
    items: z.array(itemSchema),
    offset: z.number().int().nonnegative(),
    total: z.number().int().nonnegative(),
  })
  .strict();
export type ListLocationWindowResponse = z.infer<
  typeof listLocationWindowSchema
>;

export const initialListLocationSchema = listLocationWindowSchema.extend({
  complete: z.boolean(),
  focusIndex: z.number().int().nonnegative().nullable(),
  selected: z.array(resolvedPathSchema),
});
export type InitialListLocationResponse = z.infer<
  typeof initialListLocationSchema
>;

export const listingFileNeighborSchema = listLocationWindowSchema
  .extend({
    focusIndex: z.number().int().nonnegative(),
  })
  .nullable();
export type ListingFileNeighborResponse = z.infer<
  typeof listingFileNeighborSchema
>;

// Mirrors the serde-tagged Rust enum; only codes cross the boundary.
export const listErrorSchema = z.discriminatedUnion("code", [
  z.object({ code: z.literal("not-found") }).strict(),
  z.object({ code: z.literal("not-a-directory") }).strict(),
  z.object({ code: z.literal("permission-denied") }).strict(),
  z.object({ code: z.literal("session-expired") }).strict(),
  z.object({ code: z.literal("io") }).strict(),
]);
export type ListErrorPayload = z.infer<typeof listErrorSchema>;
