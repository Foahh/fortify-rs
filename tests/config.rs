use fortify::config::{Config, safe_folder};
use std::path::Path;

#[test]
fn bundled_config_is_valid_and_defaults_are_enabled() {
    let config = Config::parse(fortify::config::DEFAULT_CONFIG, None).unwrap();

    assert!(config.raw.sort && config.raw.duplicates && config.raw.prune);
    assert_eq!(
        config.destination(Path::new("REPORT.PDF")).0,
        Path::new("Documents")
    );

    assert_eq!(
        config.destination(Path::new("archive.r01")).0,
        Path::new("Compressed")
    );

    assert_eq!(
        config.destination(Path::new("unknown.zzz")).0,
        Path::new("Others")
    );
}

#[test]
fn configuration_rejects_invalid_inputs() {
    for text in [
        "",
        "version=2",
        "version=1\nignore='wrong'",
        "version=1\nunknown=true",
        "version=1\nignore=['[']",
        "version=1\n[mapping]\nDocs='pdf'",
        "version=1\n[mapping]\nDocs=[]",
        "version=1\n[mapping]\nDocs=['']",
        "version=1\n[mapping]\nDocs=['pdf']\nOther=['PDF']",
        "version=1\n[[rules]]\nname=''\nmatch=['*']\nfolder='Docs'",
        "version=1\n[[rules]]\nname='a'\nmatch=[]\nfolder='Docs'",
        "version=1\n[[rules]]\nname='a'\nmatch=['dir/*']\nfolder='Docs'",
        "version=1\n\
         [[rules]]\n\
         name='a'\n\
         match=['*']\n\
         folder='Docs'\n\
         [[rules]]\n\
         name='a'\n\
         match=['x']\n\
         folder='Other'",
    ] {
        assert!(Config::parse(text, None).is_err(), "accepted {text:?}");
    }
}

#[test]
fn destinations_reject_escapes_and_reserved_paths_on_every_platform() {
    for path in [
        "",
        ".",
        "..",
        "../outside",
        "Docs/../outside",
        "/absolute",
        "\\absolute",
        "C:\\outside",
        "C:relative",
        "a//b",
        "a\\..\\b",
        ".fortify/data",
        "duplicates",
        "Docs/Duplicates",
        "CON",
        "nul.txt",
        "COM1",
        "a.",
        "a ",
        "a:b",
        "a?b",
    ] {
        assert!(safe_folder(path).is_err(), "accepted {path:?}");
    }

    assert_eq!(
        safe_folder("Pictures\\Screenshots").unwrap(),
        Path::new("Pictures/Screenshots")
    );
}

#[test]
fn ordered_rules_exact_extensions_and_globs_have_defined_precedence() {
    let config = Config::parse(
        "version=1\n\
         [mapping]\n\
         First=['r*']\n\
         Second=['r0*']\n\
         Exact=['r01']\n\
         [[rules]]\n\
         name='first'\n\
         match=['Special*']\n\
         folder='Named'\n\
         [[rules]]\n\
         name='second'\n\
         match=['*']\n\
         folder='Fallback'\n",
        None,
    )
    .unwrap();

    assert_eq!(
        config.destination(Path::new("Special.r01")).0,
        Path::new("Named")
    );

    assert_eq!(
        config.destination(Path::new("special.r01")).0,
        Path::new("Fallback")
    );
    let config = Config::parse(
        "version=1\n[mapping]\nFirst=['r*']\nSecond=['r0*']\nExact=['r01']",
        None,
    )
    .unwrap();

    assert_eq!(
        config.destination(Path::new("archive.r01")).0,
        Path::new("Exact")
    );

    assert_eq!(
        config.destination(Path::new("archive.r02")).0,
        Path::new("First")
    );
}

#[test]
fn ignores_use_basename_and_target_relative_paths() {
    let config = Config::parse("version=1\nignore=['*.tmp','Pictures/private/**']", None).unwrap();

    assert!(config.ignored(Path::new("Documents/file.tmp")));
    assert!(config.ignored(Path::new("Pictures/private/photo.png")));
    assert!(!config.ignored(Path::new("Pictures/public/photo.png")));
}
