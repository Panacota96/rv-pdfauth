> La version de référence est la version anglaise ([installation.md](installation.md)) ; cette traduction la suit.

# Installation et vérification (Windows 10/11)

RV PDFAuth s'exécute **localement** sur le PC Windows 10 ou 11 de l'expert (x64). Il n'a besoin ni de droits
d'administrateur, ni d'environnement d'exécution (runtime C statique), ni de réseau, sauf pour l'horodatage RFC 3161 facultatif et la vérification des versions facultative. Cette page couvre :
obtenir une version, **prouver qu'il s'agit de la compilation authentique**, l'installer (zip portable ou programme
d'installation par utilisateur), vérifier le moteur de rendu PDFium, l'autotest de la première exécution, l'usage hors
ligne, la compilation depuis les sources pour les auditeurs, l'emplacement des fichiers et la désinstallation.

Pourquoi la vérification compte : un tribunal peut demander comment vous savez que le programme exécuté est celui dont
un expert a examiné le code source. Les étapes 2–4 et 7 apportent cette réponse ; conservez leur sortie avec le
dossier ([admissibility.md](admissibility.md#examiner-checklist-before-filing-a-report), étape 3, en anglais).

```mermaid
flowchart TB
    A["1 · Télécharger les fichiers de la version<br/>zip ou setup.exe + SHA256SUMS"] --> B["2 · SHA-256 de chaque fichier<br/>= SHA256SUMS ?"]
    B -->|non| X["Arrêt : ne pas l'utiliser<br/>retélécharger, signaler"]
    B -->|oui| C["3 · Provenance<br/>gh attestation verify"]
    C -->|échec| X
    C -->|ok| D["4 · Authenticode<br/>(quand la version est signée)"]
    D --> E{"5 · Installer"}
    E -->|portable| F["Débloquer + extraire le zip"]
    E -->|programme d'installation| G["setup.exe par utilisateur<br/>sans administrateur"]
    F & G --> H["6 · Vérifier PDFium<br/>SHA-256 + version de pdfium.dll"]
    H --> I["7 · self-test + exécution d'exemple<br/>résumé attendu, verify-log"]
    I -->|ok| J["Prêt pour les affaires<br/>conservez les sorties"]
    I -->|différent| X
```

## Prérequis

| Élément | Exigence |
| --- | --- |
| Système d'exploitation | Windows 10 ou 11, x64 (`MinVersion=10.0` dans le programme d'installation). Linux/macOS : compiler depuis les sources. |
| Droits | Utilisateur standard. Le programme d'installation est par utilisateur (`PrivilegesRequired=lowest`). |
| Disque | Les deux exécutables plus `pdfium.dll` (7,2 Mo) et la documentation ; les sorties demandent de la place pour les révisions (jusqu'à la taille de l'élément par révision), les rendus (de quelques dizaines à quelques centaines de ko par page modifiée) et les rapports. |
| Mémoire | Chaque PDF est analysé dans un processus d'analyse limité par défaut à 2048 Mio (`--worker-memory`). Une machine de 8 Go suffit. |
| Graphisme | Rien n'est requis : la GUI se rabat sur le rendu logiciel (WARP) sur les machines virtuelles sans pilote GPU. |
| Réseau | Aucun, sauf la demande d'horodatage facultative (voir l'[étape 8](#8-usage-hors-ligne-et-sur-poste-isolé)). |
| Vérification des versions | Facultative, simple notification : l'application de bureau au démarrage et `rv-pdfauth update-check` lisent la page publique de la dernière version sur github.com ([Vérification des versions](#vérification-des-versions)) ; `RV_NO_UPDATE_CHECK=1` la désactive. |

## 1. Téléchargement

Les fichiers de la version sont sur la page Releases GitHub du dépôt (`Panacota96/rv-pdfauth` à ce commit) :

| Fichier | Ce que c'est |
| --- | --- |
| `rv-pdfauth-X.Y.Z-windows-x64.zip` | Portable : `rv-pdfauth.exe`, `rv-pdfauth-gui.exe`, `pdfium.dll`, `licenses/pdfium/`, README, LICENSE, COMMERCIAL.md, NOTICE, CHANGELOG, `docs/`, `examples/` |
| `rv-pdfauth-X.Y.Z-windows-x64-setup.exe` | Programme d'installation Inno Setup, par utilisateur |
| `SHA256SUMS` | SHA-256 du zip, du programme d'installation et du SBOM (format `sha256sum` : `<hash>  ./<file>`) |
| `sbom.cdx.json` | Nomenclature logicielle CycloneDX (chaque dépendance Rust et sa version) |

Placez-les dans un même dossier, par ex. `C:\Rakvor\downloads\rv-pdfauth-0.1.0\`.

## 2. Vérifier le téléchargement

```powershell
cd C:\Rakvor\downloads\rv-pdfauth-0.1.0
Get-Content .\SHA256SUMS | ForEach-Object {
    $hash, $name = $_ -split '\s+', 2
    $name = $name.TrimStart('*').Replace('./', '')
    if (Test-Path $name) {
        $actual = (Get-FileHash -Algorithm SHA256 $name).Hash.ToLower()
        if ($actual -eq $hash) { "OK    $name" } else { "FAIL  $name  (got $actual)" }
    } else { "skip  $name (not downloaded)" }
}
```

Chaque fichier téléchargé doit afficher `OK`. Un fichier à la fois :

```powershell
Get-FileHash .\rv-pdfauth-0.1.0-windows-x64.zip -Algorithm SHA256
```

`SHA256SUMS` lui-même n'est protégé qu'indirectement par l'étape 3 (il est produit par le même workflow) ;
l'attestation de l'étape 3 est le contrôle le plus fort.

## 3. Vérifier la provenance de la compilation

Chaque zip et programme d'installation porte une attestation de provenance de compilation GitHub
(`actions/attest-build-provenance`) : une déclaration signée que ce fichier exact a été compilé par le workflow de
publication du dépôt à partir d'un commit donné. Sur une machine avec accès à internet et la CLI GitHub :

```powershell
gh attestation verify .\rv-pdfauth-0.1.0-windows-x64.zip --repo Panacota96/rv-pdfauth
gh attestation verify .\rv-pdfauth-0.1.0-windows-x64-setup.exe --repo Panacota96/rv-pdfauth
```

La sortie nomme le workflow (`.github/workflows/release.yml`) et le commit source ; consignez les deux. Pour un
laboratoire isolé, vérifiez sur une machine connectée et transportez les fichiers vérifiés sur un support protégé en
écriture (la CLI GitHub peut aussi télécharger le paquet d'attestation et la racine de confiance pour une vérification
hors ligne ; voir `gh attestation verify --help`). `SHA256SUMS` et le SBOM ne sont pas attestés eux-mêmes.

## 4. Vérifier la signature Authenticode (versions signées)

Le workflow de publication signe `rv-pdfauth.exe`, `rv-pdfauth-gui.exe` et le programme d'installation **seulement
quand un certificat de signature de code est configuré** pour le dépôt. Les versions sont signées avec le certificat de
signature de code **interne** de Rakvor (« Rakvor Code Signing (internal) », émis par « Rakvor Internal Code Signing
Root CA »). Ce n'est pas un certificat public : Windows ne lui fait confiance que sur les machines où la racine Rakvor
est installée (les portables de Rakvor, puisque l'outil est utilisé localement et n'est pas distribué). Les auditeurs
et les clients vérifient les versions avec les étapes 2–3 et peuvent examiner la signature avec l'empreinte de la
racine ci-dessous.

Installez la racine une fois par machine Rakvor (PowerShell, avec l'utilisateur qui exécute l'outil ; le fichier est
`packaging/signing/rakvor-code-signing-root.cer` dans le dépôt) :

```powershell
# Vérifiez d'abord l'empreinte ; elle doit être exactement :
#   SHA-256 DC9BA1A2FCD4F0633C5214B7CCEB25BED8F3F7D9EBAF111590250CBCAA83DF60
(New-Object System.Security.Cryptography.X509Certificates.X509Certificate2 .\rakvor-code-signing-root.cer).GetCertHashString([System.Security.Cryptography.HashAlgorithmName]::SHA256)
Import-Certificate -FilePath .\rakvor-code-signing-root.cer -CertStoreLocation Cert:\CurrentUser\Root
```

Windows affiche une boîte de confirmation pour la nouvelle racine ; acceptez-la seulement si l'empreinte correspond.
Sans la racine, `Get-AuthenticodeSignature` indique `UnknownError` (chaîne vers une racine non approuvée) et SmartScreen
peut encore avertir à la première exécution. Vérifiez :

```powershell
Get-AuthenticodeSignature .\rv-pdfauth-0.1.0-windows-x64-setup.exe |
    Format-List Status, StatusMessage, SignerCertificate, TimeStamperCertificate
# après extraction du zip :
Get-AuthenticodeSignature .\rv-pdfauth.exe, .\rv-pdfauth-gui.exe | Format-Table Status, Path
```

| État | Signification |
| --- | --- |
| `Valid` | Signé et intact ; le signataire doit être « Rakvor Code Signing (internal) » |
| `UnknownError` | Signé, mais la racine Rakvor n'est pas installée sur cette machine (installez-la comme ci-dessus) |
| `NotSigned` | La version a été compilée sans certificat de signature : appuyez-vous sur les étapes 2–3 ; Windows SmartScreen avertira |
| `HashMismatch` / autre | Arrêt : le fichier a été modifié après la signature |

`pdfium.dll` est une compilation tierce (bblanchon/pdfium-binaries) et n'est **pas** signée Authenticode ; elle est
vérifiée par son SHA-256 à l'étape 6. Avec le SDK Windows, `signtool verify /pa /v rv-pdfauth.exe` donne la chaîne
complète.

## 5. Installer

### 5a. Zip portable

```powershell
# seulement après les étapes 2–4
Unblock-File .\rv-pdfauth-0.1.0-windows-x64.zip
Expand-Archive .\rv-pdfauth-0.1.0-windows-x64.zip -DestinationPath C:\Rakvor\tools\rv-pdfauth-0.1.0
cd C:\Rakvor\tools\rv-pdfauth-0.1.0
.\rv-pdfauth.exe --version
```

Gardez un dossier par version, pour pouvoir relancer une ancienne affaire avec la version exacte consignée dans son
`environment.json`. Facultatif, pour la session en cours seulement : `$env:Path += ";C:\Rakvor\tools\rv-pdfauth-0.1.0"`.

### 5b. Programme d'installation (par utilisateur, sans administrateur)

Double-cliquez sur `rv-pdfauth-X.Y.Z-windows-x64-setup.exe` (assistant en anglais, espagnol ou français). Il installe
dans `%LOCALAPPDATA%\Programs\Rakvor\RV PDFAuth\`, crée un groupe *Rakvor* dans le menu Démarrer (application +
documentation) et propose deux tâches : **Add the rv-pdfauth command to PATH** (PATH de l'utilisateur, cochée à la
première installation) et une icône sur le bureau. Ouvrez ensuite un **nouveau** terminal pour que le PATH s'applique.

Sans intervention :

```powershell
.\rv-pdfauth-0.1.0-windows-x64-setup.exe /VERYSILENT /SUPPRESSMSGBOXES /NORESTART /TASKS="addtopath"
```

Le programme d'installation copie les exécutables, `pdfium.dll`, les licences, `docs\` et les éléments d'exemple
(`examples\sample\evidence\`), pas les scripts d'exemple ni `expected_hashes.csv` (ils sont dans le zip et l'arbre des
sources).

## 6. Vérifier PDFium (moteur de rendu de la comparaison visuelle)

La comparaison visuelle charge `pdfium.dll` **à l'exécution** depuis, dans l'ordre : le chemin de `RV_PDFIUM_PATH`
(fichier ou dossier), le dossier de l'exécutable, le dossier parent de l'exécutable. Le dossier de travail courant
n'est jamais consulté (pas d'implantation de DLL depuis un dossier d'éléments de preuve). Sans la bibliothèque, chaque
analyse se termine quand même, et les PDF à plusieurs révisions reçoivent `VISUAL_DIFF_SKIPPED` avec la raison.

Compilation fixée (d'après [third_party/pdfium/PDFIUM_VERSION.txt](../third_party/pdfium/PDFIUM_VERSION.txt)) :

| Champ | Valeur |
| --- | --- |
| Source | <https://github.com/bblanchon/pdfium-binaries>, étiquette `chromium/7881` |
| Version | 151.0.7881.0 |
| SHA-256 de l'archive `pdfium-win-x64.tgz` | `73cc0de638ac2095e7445bf56a38200a5b7c7ca0e9f4ba144598f2457377ac08` |
| SHA-256 de `bin/pdfium.dll` qu'elle contient | `79d4676b656cfb1abcea88f9ade3b4b0826c5200382db5f4ec72a636c598c118` (7 211 520 octets) |

```powershell
cd "$env:LOCALAPPDATA\Programs\Rakvor\RV PDFAuth"      # ou le dossier portable
(Get-FileHash .\pdfium.dll -Algorithm SHA256).Hash.ToLower()  # doit être égal à 79d4676b…c118
(Get-Item .\pdfium.dll).VersionInfo.FileVersion                # 151.0.7881.0
```

Pour recalculer vous-même la valeur de référence (tout système avec `tar`) :

```powershell
Invoke-WebRequest "https://github.com/bblanchon/pdfium-binaries/releases/download/chromium%2F7881/pdfium-win-x64.tgz" -OutFile pdfium-win-x64.tgz
(Get-FileHash .\pdfium-win-x64.tgz -Algorithm SHA256).Hash.ToLower()   # 73cc0de6…ac08
tar -xzf .\pdfium-win-x64.tgz bin/pdfium.dll
(Get-FileHash .\bin\pdfium.dll -Algorithm SHA256).Hash.ToLower()       # 79d4676b…c118
```

Chaque exécution consigne aussi la bibliothèque réellement chargée (chemin, SHA-256, étiquette et version fixées) dans
l'entrée d'audit `item_analyzed` de chaque PDF (`details.analysis.visual.renderer`).

## 7. Autotest de la première exécution et exemple

```powershell
rv-pdfauth --version
rv-pdfauth self-test
```

`self-test` affiche `PASS` pour md5, sha1, sha256, sha512, sha3-256 et blake3 (réponses connues pour `"abc"`), avec le
code de sortie 0. Il ne teste que les implémentations d'empreinte. Lancez ensuite l'exemple synthétique, qui met en
œuvre l'analyse PDF, PDFium et la chaîne d'audit :

```powershell
# zip portable (script inclus) : attend le code de sortie 3, vérifie le journal et les empreintes de l'exemple
.\examples\sample\run.ps1 -Bin .\rv-pdfauth.exe

# programme d'installation (le script n'est pas installé) : lancez-le à la main dans un dossier temporaire
$app = "$env:LOCALAPPDATA\Programs\Rakvor\RV PDFAuth"
$out = Join-Path $env:TEMP "rv-pdfauth-check"
rv-pdfauth analyze -i "$app\examples\sample\evidence" --hash sha256,md5 --out $out --progress none --no-timestamp
$LASTEXITCODE                                  # 3 = terminé avec des constatations de gravité élevée (voulu)
rv-pdfauth verify-log (Get-ChildItem "$out\logs" | Select-Object -Last 1).FullName   # OK … chain intact
```

Résumé attendu avec PDFium présent : **7 items, 0 errors, findings high 3, medium 18, low 3, info 5**, code de sortie
3. Sans PDFium, les comptes diffèrent et `VISUAL_DIFF_SKIPPED` apparaît : corrigez l'étape 6 avant de traiter des
affaires. Conservez la sortie de la console et le dossier de l'exécution comme trace de cette vérification.

## 8. Usage hors ligne et sur poste isolé

Outre la vérification des versions facultative ([Vérification des versions](#vérification-des-versions)), le **seul** accès réseau du code est la demande d'horodatage RFC 3161 (un HTTP POST d'une empreinte de 32 octets
vers la TSA, [src/core/src/trust.rs](../src/core/src/trust.rs)). Il n'y a ni télémétrie ni contrôle de licence ;
l'apprentissage des empreintes est local.

| Pour désactiver l'horodatage | Comment |
| --- | --- |
| Une exécution de la CLI | `--no-timestamp` |
| Toutes les exécutions de la CLI sur ce PC / cette session | `$env:RV_NO_TIMESTAMP = "1"` (ou définissez-la comme variable d'environnement utilisateur) |
| GUI | décochez **5. Confiance → Horodatage qualifié** avant *Lancer l'analyse* |

Une exécution hors ligne avec l'horodatage activé se termine quand même : elle consigne `timestamp_pending` et le
rapport de statistiques affiche un avertissement. Plus tard, sur une machine qui peut joindre la TSA, lancez
`rv-pdfauth timestamp <log folder>` sur le dossier des journaux de l'exécution (il revérifie la chaîne, puis ajoute
`timestamp_obtained`). Pour utiliser une TSA interne ou qualifiée, passez `--tsa-url` (voir
[admissibility.md](admissibility.md#53-eidas--regulation-eu-9102014-as-amended-by-eu-20241183), en anglais).
Malgré son libellé dans l'interface, cet horodatage n'est qualifié au sens d'eIDAS que si l'adresse est celle d'un
prestataire de services de confiance qualifié (QTSP) inscrit sur une liste de confiance de l'UE ; l'adresse par défaut
(DigiCert) ne l'est pas.
Le processus d'analyse PDF n'a pas de code réseau. Sous Linux, il s'exécute en plus dans un espace de noms réseau vide
quand le noyau le permet (consigné par élément comme `worker.network` dans le journal d'audit) ; sous Windows, il n'y a
pas de blocage réseau au niveau du système pour lui (écart G23) : sur un poste isolé cela n'a pas d'importance ;
ailleurs, une règle sortante du Pare-feu Windows pour `rv-pdfauth.exe` / `rv-pdfauth-gui.exe` qui n'autorise que
l'hôte de la TSA l'impose.

## 9. Compiler depuis les sources pour les auditeurs

### Chaîne de compilation

| Élément | Version |
| --- | --- |
| Rust | Installez-le avec `rustup`. Minimum **1.88** pour la CLI et les crates du cœur (chaînes `let` de l'édition 2024 ; chaque dépendance hors GUI de `Cargo.lock` déclare ≤ 1.85) et **1.95** pour la GUI (egui/eframe 0.37) : les valeurs `rust-version` de `Cargo.toml` et `src/gui/Cargo.toml`, vérifiées en CI par le job `msrv`. Pour reproduire une version, utilisez le compilateur exact de l'`environment.json` de cette version → `tool.rustc` (par ex. `rustc 1.97.0 (2d8144b78 2026-07-07)`) : `rustup toolchain install 1.97.0 --profile minimal` et compilez avec `cargo +1.97.0 …`. Si l'arbre des sources contient `rust-toolchain.toml`, rustup le choisit automatiquement. |
| Éditeur de liens Windows | Visual Studio 2022 Build Tools, charge de travail *Desktop development with C++* (MSVC + Windows SDK) ; `rustup default stable-msvc`. Le CRT statique est fixé dans `.cargo/config.toml`. |
| Linux | Aucun paquet système n'est nécessaire pour compiler la CLI. |
| Facultatif | Python 3.12 avec `blake3 jsonschema pyhanko` pour les vérificateurs indépendants et les tests de confiance ; `pikepdf` pour l'oracle PDF. |

### Compilation

```powershell
git clone https://github.com/Panacota96/rv-pdfauth.git
cd rv-pdfauth
git checkout v0.1.0                       # l'étiquette de la version examinée
cargo build --release --locked -p rv-pdfauth-cli -p rv-pdfauth-gui
.\scripts\fetch_pdfium.ps1 -Platform win-x64     # télécharge chromium/7881, s'arrête si le SHA-256 ne correspond pas
Copy-Item third_party\pdfium\win-x64\pdfium.dll target\release\
.\target\release\rv-pdfauth.exe self-test
.\examples\sample\run.ps1 -Bin .\target\release\rv-pdfauth.exe
cargo test --workspace --locked            # tests unitaires + d'intégration (ceux de PDFium sont ignorés sans RV_PDFIUM_PATH)
```

```bash
# Linux
cargo build --release --locked -p rv-pdfauth-cli
scripts/fetch_pdfium.sh linux-x64
export RV_PDFIUM_PATH="$PWD/third_party/pdfium/linux-x64/libpdfium.so"
target/release/rv-pdfauth self-test && examples/sample/run.sh target/release/rv-pdfauth
python tests/oracle_pdf.py                 # facultatif : recoupement avec pikepdf/qpdf et pyHanko
```

`--locked` refuse de compiler si `Cargo.lock` devait changer : l'ensemble des dépendances est donc celui listé dans
`environment.json` → `dependencies` et dans `sbom.cdx.json`.

### Reproduire les empreintes de la version

Soyez précis sur ce qui peut ou non correspondre bit à bit dans cette version (écart G22) :

| Artefact | Identique bit à bit à la version ? | Comment le vérifier |
| --- | --- | --- |
| `pdfium.dll` | Oui (téléchargée, pas compilée) | SHA-256 `79d4676b…c118` ([étape 6](#6-vérifier-pdfium-moteur-de-rendu-de-la-comparaison-visuelle)) |
| Fichiers de `docs/`, `examples/` | Oui (depuis git ; fins de ligne LF sauf `*.ps1`, `*.iss`) | `git ls-files` + `Get-FileHash` contre le zip extrait |
| `rv-pdfauth.exe`, `rv-pdfauth-gui.exe` | **Non garanti** : le compilateur est fixé (la CI et la publication lisent `rust-toolchain.toml`, 1.97.0, le même que `environment.json` → `tool.rustc`), mais les chemins ne sont pas remappés et l'en-tête PE porte des données de l'édition de liens | L'attestation de provenance (étape 3) relie le binaire publié au workflow et au commit ; comparez ensuite le comportement (ci-dessous) |
| zip, programme d'installation | Non (horodatages de l'archive) | Attestation (étape 3) |

**Équivalence fonctionnelle** : lancez l'exemple avec le binaire publié et avec votre compilation, puis comparez les
constatations de chaque élément et les empreintes des éléments de preuve :

```bash
python - "<release-out>/reports/<RUN-ID>" "<your-out>/reports/<RUN-ID>" <<'EOF'
import glob, json, sys
def load(run):
    out = {}
    for p in sorted(glob.glob(f"{run}/items/*/technical.json")):
        t = json.load(open(p, encoding="utf-8"))
        out[p.replace("\\", "/").split("/")[-2]] = (t["hashes"], [(f["code"], f["severity"], f["params"]) for f in t["findings"]])
    return out
a, b = load(sys.argv[1]), load(sys.argv[2])
print("identical findings and digests" if a == b else "DIFFERENT: " + ", ".join(k for k in a if a.get(k) != b.get(k)))
EOF
```

Comparez aussi les PNG de la comparaison visuelle (identiques avec la même compilation de PDFium et les mêmes PPP) et
les fichiers de `revisions/`.

## 10. Où se trouvent les choses

| Quoi | Emplacement |
| --- | --- |
| Programme (programme d'installation) | `%LOCALAPPDATA%\Programs\Rakvor\RV PDFAuth\` (`rv-pdfauth.exe`, `rv-pdfauth-gui.exe`, `pdfium.dll`, `licenses\pdfium\`, `docs\`, `examples\sample\evidence\`, `unins000.exe`) |
| Programme (portable) | le dossier que vous avez extrait |
| Entrée du PATH (tâche du programme d'installation) | variable d'environnement utilisateur `Path` (`HKCU\Environment`) ; retirée par le programme de désinstallation |
| Sorties des exécutions | `--out` (par défaut `.\rakvor-out` **relatif au dossier courant**) : `logs\<RUN-ID>\`, `reports\<RUN-ID>\`. La GUI utilise son champ *Dossier de sortie* |
| Base locale d'empreintes | `%LOCALAPPDATA%\Rakvor\rv-pdfauth\fingerprints.local.json` (Linux/macOS : `$XDG_DATA_HOME/rakvor/rv-pdfauth/`, par défaut `~/.local/share/rakvor/rv-pdfauth/`) |
| Son journal des modifications | `fingerprints.audit.jsonl` dans le même dossier (chaîné par empreintes) |
| Réglages de la GUI | non conservés : chaque démarrage utilise les valeurs par défaut ; chaque modification faite avant une exécution est consignée dans le journal d'audit de cette exécution |
| Certificat de signature | là où vous le rangez ; seuls son chemin, son sujet et son empreinte SHA-256 sont journalisés, jamais le mot de passe |
| Fichiers temporaires | aucun (le processus d'analyse n'écrit que dans le dossier de rapports de l'élément) |

## 11. Désinstaller

- **Programme d'installation :** *Paramètres → Applications → Applications installées → RV PDFAuth → Désinstaller*, ou
  lancez `"%LOCALAPPDATA%\Programs\Rakvor\RV PDFAuth\unins000.exe"` (silencieux : ajoutez `/VERYSILENT`). Il retire les
  fichiers installés, les raccourcis et l'entrée du dossier d'installation dans le `Path` utilisateur
  (`HKCU\Environment`) ajoutée par la tâche *Add to PATH* ; les autres entrées de `Path` ne sont pas touchées (ouvrez
  ensuite un nouveau terminal). La CI installe et désinstalle le paquet sur `windows-latest` et échoue si l'entrée
  reste. Les versions antérieures à cette correction ne la retiraient pas ; si une ancienne entrée subsiste,
  supprimez-la avec :

  ```powershell
  $dir = "$env:LOCALAPPDATA\Programs\Rakvor\RV PDFAuth"
  $p = [Environment]::GetEnvironmentVariable("Path", "User").Split(";") | Where-Object { $_ -and $_ -ne $dir }
  [Environment]::SetEnvironmentVariable("Path", ($p -join ";"), "User")
  ```

- **Portable :** supprimez le dossier.
- **Conservés volontairement** (dans les deux cas) : les sorties des exécutions (pièces de l'affaire) et la base locale
  d'empreintes avec son journal d'audit. Avant de supprimer `%LOCALAPPDATA%\Rakvor\rv-pdfauth\`, exportez la base pour
  les dossiers (`rv-pdfauth fingerprints export --out <file>`) : les rapports passés citent son SHA-256.

## 12. Dépannage de l'installation

| Symptôme | Cause et solution |
| --- | --- |
| SmartScreen « Windows a protégé votre ordinateur » (éditeur inconnu) | SmartScreen juge un fichier sur la **réputation** d'un certificat de confiance publique ; la racine interne Rakvor n'en est pas un, donc l'avertissement apparaît même quand la signature est `Valid` (et toujours avec `NotSigned`). Seulement après la réussite des étapes 2–4 : *Informations complémentaires → Exécuter quand même*, ou retirez d'abord la marque de fichier téléchargé avec `Unblock-File .\<file>` (faites-le sur le zip **avant** de l'extraire). Il ne disparaît définitivement qu'avec un certificat public (voir `docs/release-process.md` §5). |
| `rv-pdfauth` n'est pas reconnu | La tâche PATH n'a pas été choisie, ou le terminal a été ouvert avant l'installation : ouvrez un nouveau terminal ou utilisez le chemin complet. |
| Chaque PDF à plusieurs révisions affiche `VISUAL_DIFF_SKIPPED` « PDFium library not found » | `pdfium.dll` manque à côté des exécutables, a été mise en quarantaine par l'antivirus ou n'est pas de la bonne architecture. Remettez-la et refaites l'étape 6, ou définissez `RV_PDFIUM_PATH`. |
| Fenêtre de la GUI vide ou très lente sur une machine virtuelle | Pas de pilote GPU : le rendu logiciel est utilisé automatiquement ; donnez plus de CPU à la machine virtuelle. |
| `self-test` affiche `FAIL` | N'utilisez pas cette version pour des éléments de preuve ; retéléchargez-la et refaites les étapes 2–4. |
| Le résumé de l'exemple diffère de l'étape 7 | Mauvaise version ou PDFium absent ; comparez `environment.json` aux notes de version. |

## Vérification des versions

Une version plus récente est-elle publiée ? L'outil peut vous le dire ; il ne se met jamais à jour lui-même.

| | |
| --- | --- |
| Quand | L'application de bureau vérifie une fois au démarrage, en arrière-plan (un bandeau avec la page de la version n'apparaît que si une version plus récente existe). La CLI ne vérifie qu'avec `rv-pdfauth update-check` (`--json` pour les scripts ; code 0 = la dernière, 3 = version plus récente, 1 = vérification impossible). Aucune autre commande ne contacte le réseau pour cela. RV Hub vérifie chaque outil qu'il liste. |
| Ce qui est envoyé | Un HTTPS GET de `https://github.com/Panacota96/rv-pdfauth/releases/latest` (la redirection est lue, pas suivie) avec `User-Agent: rv-pdfauth/<version>` et aucun autre en-tête, cookie ni paramètre : rien sur l'affaire, les éléments, l'expert ou la machine. |
| Ce qu'elle fait | Elle nomme la version plus récente et sa page. Elle ne télécharge ni n'installe jamais rien : téléchargez la version et vérifiez-la (`SHA256SUMS`, chaîne Authenticode jusqu'à la racine Rakvor) comme dans [Vérifier le téléchargement](#2-vérifier-le-téléchargement) avant usage. |
| Audit | La vérification ne fait pas partie d'une exécution et n'est écrite dans aucun journal d'audit ; une vérification en échec ou lente (hors ligne, délai de 5 s) ne bloque jamais l'outil. |
| La désactiver | `RV_NO_UPDATE_CHECK=1` (toute valeur sauf vide ou `0`), ou le fichier `update-check-off` dans `%APPDATA%\Rakvor` (le réglage de RV Hub l'écrit) : désactivée pour tous les outils Rakvor de ce PC. |
| Code | [src/core/src/update.rs](../src/core/src/update.rs) (partagé, identique octet pour octet, par tous les outils Rakvor) |
