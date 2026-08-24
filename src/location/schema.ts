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

export const listLocationSchema = z
  .object({
    path: z.string(),
    items: z.array(itemSchema),
  })
  .strict();
export type ListLocationResponse = z.infer<typeof listLocationSchema>;

export const initialListLocationSchema = listLocationSchema.extend({
  total: z.number().int().nonnegative(),
  complete: z.boolean(),
});
export type InitialListLocationResponse = z.infer<
  typeof initialListLocationSchema
>;

// Mirrors the serde-tagged Rust enum; only codes cross the boundary.
export const listErrorSchema = z.discriminatedUnion("code", [
  z.object({ code: z.literal("not-found") }).strict(),
  z.object({ code: z.literal("not-a-directory") }).strict(),
  z.object({ code: z.literal("permission-denied") }).strict(),
  z.object({ code: z.literal("io") }).strict(),
]);
export type ListErrorPayload = z.infer<typeof listErrorSchema>;
