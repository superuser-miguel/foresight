# Translations are extracted with xgettext from two source shapes:
#
#  - src/ui/window.blp uses _("…"), which blueprint-compiler turns into
#    translatable="yes" in the generated .ui — the --keyword=_ below is what
#    picks those up from the .blp source (xgettext treats the unknown .blp
#    extension as C, which is exactly the _("…") call shape).
#  - The Rust sources call i18n/i18n_f/i18n_k/ni18n (see crates/foresight/src/
#    i18n.rs); their strings are extracted through the --keyword=* flags below.
#    No --language is forced: letting xgettext guess per file avoids its C
#    parser choking on Rust lifetimes in the .rs files.
#
# POTFILES lists both shapes relative to the source root.
i18n.gettext(
  meson.project_name(),
  preset: 'glib',
  args: [
    '--keyword=_',
    '--keyword=i18n',
    '--keyword=i18n_f',
    '--keyword=i18n_k',
    '--keyword=ni18n:1,2',
    '--keyword=gettext_noop',
    '--keyword=gettext',
  ],
)
