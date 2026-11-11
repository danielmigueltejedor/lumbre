# Icon

Status: **not decided.** There is no official Lumbre icon yet, and none is referenced by the
README or the application.

The brief for the icon asks for an original, minimal, geometric symbol inspired by light or
energy — no generic flames, no brains with circuits, no "AI sparkle" stars, no decorative
gradients — with a recognisable variant for Lumbre Código.

Every other Nodalia application, however, uses a different system: a black-and-white portrait
of a historical figure on the application's colour (Vortia is Bernoulli, Forja is Leonardo,
Lienzo is Velázquez). That system was chosen after abstract glyphs were rejected as forgettable.
The two directions conflict, so the choice is the project owner's and has been put to him:

1. **A portrait, like the family.** Candidates with a real claim on the subject and
   public-domain likenesses: Leonardo Torres Quevedo (built *El Ajedrecista*, the first machine
   to play a chess endgame on its own, 1912), Ramon Llull (the *Ars Magna*, mechanical
   combination of concepts, 13th century), Gottfried Leibniz (the *calculus ratiocinator*).
2. **A geometric symbol of its own.** Three sketches are in
   [`design/icon-proposals`](../design/icon-proposals/lumbre-icon-options.png), each with a
   Lumbre Código variant in which the ember becomes a cursor:
   - **A · Hogar** — an arch with an ember inside: the hearth. "Lumbre" is the household fire,
     and the arch is also a doorway: the thing that stays at home.
   - **B · Ele** — the letter L holding an ember in its corner.
   - **C · Núcleo** — an ember inside a ring that is open on one side: a core that is there on
     demand.

   All use flat colours only, in two palettes: charcoal with an ember-orange accent, or an
   ember-orange field.

Also pending, tied to the same choice: Lumbre's accent colour in the Nodalia Design Language
(one line in `accents.json` of `nodalia-design`). Ember orange sits very close to Pluma's
existing accent, so the colour needs deciding together with the icon. Until then the desktop
app uses the default Nodalia accent.

Once chosen, the deliverables are: the editable SVG in `assets/icons/`, PNG at the standard
sizes, ICO and ICNS, the Lumbre Código variant, the README header, and the application and
window icons.
