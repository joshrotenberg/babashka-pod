(ns release
  "Release tooling for the pods in this workspace: packaging, binary
  architecture checks, and pod-registry manifests.

  The pod registry matches a manifest entry's :os/name and :os/arch against
  babashka's os.name and os.arch, then downloads that entry's zip. Nothing in
  that chain checks the binary itself, so a manifest written by hand can list
  an x86_64 binary under aarch64, or leave out a build that exists. Here the
  manifest is generated from the zips, and each binary's header must match
  the entry it is listed under."
  (:require [babashka.fs :as fs]
            [clojure.string :as str])
  (:import [java.nio ByteBuffer ByteOrder]
           [java.security MessageDigest]
           [java.util.zip ZipFile]))

;; --- binary headers ----------------------------------------------------------

(def ^:private macho-cpus {0x01000007 "x86_64" 0x0100000C "aarch64"})
(def ^:private elf-machines {0x3E "x86_64" 0xB7 "aarch64" 0x03 "x86" 0x28 "arm"})
(def ^:private pe-machines {0x8664 "x86_64" 0xAA64 "aarch64" 0x14C "x86"})

(defn- buf ^ByteBuffer [^bytes bs order]
  (.order (ByteBuffer/wrap bs) order))

(defn- u16 [^bytes bs offset order]
  (bit-and (.getShort (buf bs order) (int offset)) 0xFFFF))

(defn- u32 [^bytes bs offset order]
  (bit-and (.getInt (buf bs order) (int offset)) 0xFFFFFFFF))

(defn header
  "Reads an executable's header bytes. Returns {:os \"macos\"|\"linux\"|\"windows\"
  :cpus #{...}}, or nil for bytes that are not a Mach-O, ELF, or PE file.
  A universal (fat) Mach-O lists every CPU it contains."
  [^bytes bs]
  (let [n (alength bs)
        le ByteOrder/LITTLE_ENDIAN
        be ByteOrder/BIG_ENDIAN
        cpu-name (fn [table k] (get table k (format "unknown-0x%x" k)))]
    (cond
      ;; 64-bit Mach-O, little-endian: CF FA ED FE.
      (and (>= n 8) (= 0xFEEDFACF (u32 bs 0 le)))
      {:os "macos" :cpus #{(cpu-name macho-cpus (u32 bs 4 le))}}

      ;; Universal binary: CA FE BA BE, big-endian, then fat_arch entries
      ;; of 20 bytes whose first field is the CPU type.
      (and (>= n 8) (= 0xCAFEBABE (u32 bs 0 be)))
      (let [count (u32 bs 4 be)]
        {:os "macos"
         :cpus (set (for [i (range count)
                          :let [off (+ 8 (* 20 i))]
                          :when (<= (+ off 4) n)]
                      (cpu-name macho-cpus (u32 bs off be))))})

      ;; ELF: 7F 'E' 'L' 'F'; byte 5 gives endianness; e_machine at 18.
      (and (>= n 20) (= [0x7F 0x45 0x4C 0x46] (map #(bit-and % 0xFF) (take 4 bs))))
      (let [order (if (= 2 (aget bs 5)) be le)]
        {:os "linux" :cpus #{(cpu-name elf-machines (u16 bs 18 order))}})

      ;; PE: "MZ", e_lfanew at 0x3C points at "PE\0\0" then Machine.
      (and (>= n 0x40) (= [0x4D 0x5A] (map #(bit-and % 0xFF) (take 2 bs))))
      (let [pe (u32 bs 0x3C le)]
        (when (and (<= (+ pe 6) n) (= 0x00004550 (u32 bs pe le)))
          {:os "windows" :cpus #{(cpu-name pe-machines (u16 bs (+ pe 4) le))}})))))

(defn- head-bytes
  "The first 64 KB of a file or zip entry: enough for every header above."
  ^bytes [^java.io.InputStream in]
  (with-open [in in]
    (let [bs (.readNBytes in (* 64 1024))]
      bs)))

(defn file-header [path]
  (header (head-bytes (java.io.FileInputStream. (str path)))))

;; --- platforms ---------------------------------------------------------------

;; Each platform: the Rust target, the registry's :os/name and :os/arch (as
;; babashka reports os.name and os.arch), the slug used in artifact names,
;; and what the binary's header must say.
(def platforms
  [{:target "x86_64-unknown-linux-gnu"  :os/name "Linux.*"   :os/arch "amd64"   :slug "linux-amd64"    :header {:os "linux" :cpu "x86_64"}}
   {:target "aarch64-unknown-linux-gnu" :os/name "Linux.*"   :os/arch "aarch64" :slug "linux-aarch64"  :header {:os "linux" :cpu "aarch64"}}
   {:target "aarch64-apple-darwin"      :os/name "Mac.*"     :os/arch "aarch64" :slug "macos-aarch64"  :header {:os "macos" :cpu "aarch64"}}
   {:target "x86_64-apple-darwin"       :os/name "Mac.*"     :os/arch "x86_64"  :slug "macos-x86_64"   :header {:os "macos" :cpu "x86_64"}}
   {:target "x86_64-pc-windows-msvc"    :os/name "Windows.*" :os/arch "amd64"   :slug "windows-amd64"  :header {:os "windows" :cpu "x86_64"}}])

(defn platform-for-target [target]
  (or (first (filter #(= target (:target %)) platforms))
      ;; The GNU Windows target builds the same kind of binary.
      (when (= target "x86_64-pc-windows-gnu")
        (assoc (platform-for-target "x86_64-pc-windows-msvc") :target target))
      (throw (ex-info (str "no platform for target " target
                           "; known: " (str/join ", " (map :target platforms)))
                      {:babashka/exit 1}))))

(defn- platform-for-slug [slug]
  (first (filter #(= slug (:slug %)) platforms)))

(defn- executable [pod platform]
  (str pod (when (= "windows" (get-in platform [:header :os])) ".exe")))

(defn header-problem
  "nil when header h matches the platform, or a description of the mismatch."
  [h platform]
  (let [{:keys [os cpu]} (:header platform)]
    (cond
      (nil? h) "not a Mach-O, ELF, or PE executable"
      (not= os (:os h)) (format "built for %s, listed as %s" (:os h) os)
      (not (contains? (:cpus h) cpu))
      (format "built for %s, listed as %s" (str/join "+" (sort (:cpus h))) cpu))))

;; --- pods ----------------------------------------------------------------

(defn- unescape [s]
  (str/replace s #"\\(.)" "$1"))

(defn toml-strings
  "The string values in a Cargo.toml as {section {key value}}, with nil for
  keys before the first section. Reads only one-line `key = \"value\"`
  entries, which is all [package] and [package.metadata.pod-registry] need."
  [text]
  (loop [[line & more] (str/split-lines text)
         section nil
         acc {}]
    (if (nil? line)
      acc
      (let [line (str/trim line)]
        (if-let [[_ header] (re-matches #"\[\s*([^\[\]]+?)\s*\]\s*(?:#.*)?" line)]
          (recur more header acc)
          (if (str/starts-with? line "[[")
            (recur more ::array acc)
            (if-let [[_ k v] (re-matches #"([A-Za-z0-9_-]+)\s*=\s*\"((?:[^\"\\]|\\.)*)\"\s*(?:#.*)?" line)]
              (recur more section (assoc-in acc [section k] (unescape v)))
              (recur more section acc))))))))

(defn pod-info
  "What the release tooling needs from pods/<pod>/Cargo.toml: the crate
  name (which must be pod-<pod>), version, and pod-registry name and
  description from [package.metadata.pod-registry]. The description falls
  back to the package description."
  [pod]
  (let [path (fs/path "pods" pod "Cargo.toml")
        _    (when-not (fs/exists? path)
               (throw (ex-info (str "no pod " pod ": " path " does not exist") {:babashka/exit 1})))
        toml (toml-strings (slurp (str path)))
        pkg  (get toml "package")
        reg  (get toml "package.metadata.pod-registry")
        crate (get pkg "name")]
    (when-not (= crate (str "pod-" pod))
      (throw (ex-info (format "%s: package name is %s; pods/%s must be the crate pod-%s"
                              path crate pod pod)
                      {:babashka/exit 1})))
    (when-not (get pkg "version")
      (throw (ex-info (str "no version in " path) {:babashka/exit 1})))
    {:pod           pod
     :crate         crate
     :version       (get pkg "version")
     :registry-name (get reg "name")
     :description   (or (get reg "description") (get pkg "description"))}))

(defn crate-version
  "The [package] version in pods/<pod>/Cargo.toml."
  [pod]
  (:version (pod-info pod)))

(defn parse-tag
  "Splits a release tag pod-<name>-v<version> into {:pod name :version v},
  or returns nil."
  [tag]
  (when-let [[_ pod version] (and tag (re-matches #"pod-([a-z0-9][a-z0-9-]*)-v(\d+\.\d+\.\d+(?:[-+][0-9A-Za-z.+-]+)?)" tag))]
    {:pod pod :version version}))

(defn- checked-tag
  "The pod-info for a release tag, after checking that the tag names an
  existing pod and that pod's Cargo.toml version."
  [tag]
  (let [{:keys [pod version]} (or (parse-tag tag)
                                  (throw (ex-info (format "tag %s is not pod-<name>-v<version>" tag)
                                                  {:babashka/exit 1})))
        info (pod-info pod)]
    (when-not (= version (:version info))
      (throw (ex-info (format "tag %s does not match pods/%s/Cargo.toml version %s (expected pod-%s-v%s)"
                              tag pod (:version info) pod (:version info))
                      {:babashka/exit 1})))
    info))

;; --- tasks -------------------------------------------------------------------

(defn arch
  "Print the OS and CPU a binary was built for, from its header"
  {:org.babashka/cli {:args->opts [:file]
                      :spec {:file {:desc "Path to an executable"}}}}
  [{:keys [file]}]
  (when-not (and file (fs/exists? file))
    (throw (ex-info "usage: bb arch <file>" {:babashka/exit 1})))
  (if-let [h (file-header file)]
    (println (format "%s: %s %s" file (:os h) (str/join "+" (sort (:cpus h)))))
    (throw (ex-info (format "%s: not a Mach-O, ELF, or PE executable" file)
                    {:babashka/exit 1}))))

(defn- sha256-hex [path]
  (let [md (MessageDigest/getInstance "SHA-256")]
    (with-open [in (java.io.FileInputStream. (str path))]
      (let [buf (byte-array 65536)]
        (loop []
          (let [n (.read in buf)]
            (when (pos? n)
              (.update md buf 0 n)
              (recur))))))
    (apply str (map #(format "%02x" (bit-and % 0xFF)) (.digest md)))))

(defn package
  "Zip a release build for the pod registry, after checking its header"
  {:org.babashka/cli {:spec {:target  {:desc "Rust target triple, e.g. aarch64-apple-darwin"}
                             :pod     {:desc "Pod directory under pods/, e.g. sqlite"}
                             :version {:desc "Defaults to the version in pods/<pod>/Cargo.toml"}
                             :dist    {:default "dist" :desc "Output directory"}}}}
  [{:keys [target pod version dist]}]
  (when-not (and target pod)
    (throw (ex-info "--target and --pod are required" {:babashka/exit 1})))
  (let [platform (platform-for-target target)
        version  (or version (crate-version pod))
        exe-name (executable (str "pod-" pod) platform)
        binary   (fs/path "target" target "release" exe-name)
        _        (when-not (fs/exists? binary)
                   (throw (ex-info (str "not built: " binary
                                        " (cargo build --release -p pod-" pod " --target " target ")")
                                   {:babashka/exit 1})))
        problem  (header-problem (file-header binary) platform)
        _        (when problem
                   (throw (ex-info (str binary ": " problem) {:babashka/exit 1})))
        zip      (fs/path dist (format "pod-%s-%s-%s.zip" pod version (:slug platform)))
        staging  (fs/create-temp-dir {:prefix "pod-package-"})]
    (fs/create-dirs dist)
    (fs/delete-if-exists zip)
    (fs/copy binary (fs/path staging exe-name))
    (fs/zip zip [(str (fs/path staging exe-name))] {:root (str staging)})
    (fs/delete-tree staging)
    (spit (str zip ".sha256") (str (sha256-hex zip) "  " (fs/file-name zip) "\n"))
    (println "packaged" (str zip))))

(defn- zip-header [zip exe-name]
  (with-open [zf (ZipFile. (str zip))]
    (when-let [entry (.getEntry zf exe-name)]
      (header (head-bytes (.getInputStream zf entry))))))

(defn- artifact-edn [{:keys [os-name os-arch url exe]}]
  (format "  {:os/name %s\n   :os/arch %s\n   :artifact/url %s\n   :artifact/executable %s}"
          (pr-str os-name) (pr-str os-arch) (pr-str url) (pr-str exe)))

(defn manifest
  "Write a pod-registry manifest.edn for the zips in dist, checking each binary"
  {:org.babashka/cli {:spec {:pod         {:desc "Pod directory under pods/, e.g. sqlite"}
                             :version     {:desc "Defaults to the version in pods/<pod>/Cargo.toml"}
                             :base-url    {:desc "Release download URL prefix, e.g. https://github.com/<owner>/<repo>/releases/download/<tag>"}
                             :name        {:desc "Registry name, :pod/name. Defaults to name in [package.metadata.pod-registry]"}
                             :description {:desc ":pod/description. Defaults to description in [package.metadata.pod-registry], then [package]"}
                             :dist        {:default "dist" :desc "Directory with the zips; manifest.edn is written here"}}}}
  [{:keys [pod version base-url name description dist]}]
  (when-not (and pod base-url)
    (throw (ex-info "--pod and --base-url are required" {:babashka/exit 1})))
  (let [info        (when-not (and version name description) (pod-info pod))
        version     (or version (:version info))
        name        (or name (:registry-name info)
                        (throw (ex-info (str "no --name, and no name in [package.metadata.pod-registry] of pods/" pod "/Cargo.toml")
                                        {:babashka/exit 1})))
        description (or description (:description info))
        prefix   (format "pod-%s-%s-" pod version)
        zips     (->> (fs/glob dist (str prefix "*.zip")) (map str) sort)
        checked  (for [zip zips
                       :let [slug     (-> (fs/file-name zip) (subs (count prefix)) (str/replace #"\.zip$" ""))
                             platform (platform-for-slug slug)
                             exe      (when platform (executable (str "pod-" pod) platform))
                             problem  (cond
                                        (nil? platform) (str "unknown platform " slug)
                                        :else (let [h (zip-header zip exe)]
                                                (if (nil? h)
                                                  (str "no executable " exe " in the zip, or not a binary")
                                                  (header-problem h platform))))]]
                   {:zip zip :platform platform :exe exe :problem problem})
        problems (filter :problem checked)]
    (when (empty? zips)
      (throw (ex-info (str "no " prefix "*.zip files in " dist) {:babashka/exit 1})))
    (doseq [{:keys [zip platform problem]} checked]
      (println (format "  %-4s %-48s %s" (if problem "FAIL" "ok") (fs/file-name zip)
                       (or problem (str (:os/name platform) " " (:os/arch platform))))))
    (when (seq problems)
      (throw (ex-info (str (count problems) " artifact(s) do not match their manifest entry")
                      {:babashka/exit 1})))
    (let [order    (into {} (map-indexed (fn [i p] [(:slug p) i]) platforms))
          entries  (->> checked
                        (sort-by #(order (:slug (:platform %))))
                        (map (fn [{:keys [zip platform exe]}]
                               {:os-name (:os/name platform)
                                :os-arch (:os/arch platform)
                                :url     (str (str/replace base-url #"/+$" "") "/" (fs/file-name zip))
                                :exe     exe})))
          missing  (remove (set (map (comp :slug :platform) checked)) (map :slug platforms))
          out      (fs/path dist "manifest.edn")]
      (spit (str out)
            (str "{:pod/name " name "\n"
                 " :pod/description " (pr-str description) "\n"
                 " :pod/version " (pr-str version) "\n"
                 " :pod/license \"MIT OR Apache-2.0\"\n"
                 " :pod/language \"rust\"\n"
                 " :pod/artifacts\n"
                 " [" (str/triml (str/join "\n" (map artifact-edn entries))) "]}\n"))
      (when (seq missing)
        (println "  note: no artifact for" (str/join ", " missing)))
      (println "wrote" (str out) "with" (count entries) "artifact(s)"))))

(defn check-version
  "Fail unless a release tag names a pod and matches its Cargo.toml version"
  {:org.babashka/cli {:args->opts [:tag]
                      :spec {:tag {:desc "Tag such as pod-sqlite-v0.1.0"}}}}
  [{:keys [tag]}]
  (let [{:keys [pod version]} (checked-tag tag)]
    (println (format "tag %s matches pods/%s/Cargo.toml version %s" tag pod version))))

(defn release-plan
  "Check a release tag and print pod=, crate= and version= lines for $GITHUB_OUTPUT"
  {:org.babashka/cli {:args->opts [:tag]
                      :spec {:tag {:desc "Tag such as pod-sqlite-v0.1.0"}}}}
  [{:keys [tag]}]
  (let [{:keys [pod crate version]} (checked-tag tag)]
    (println (str "pod=" pod))
    (println (str "crate=" crate))
    (println (str "version=" version))))
