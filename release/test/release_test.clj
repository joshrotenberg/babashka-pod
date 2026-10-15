(ns release-test
  (:require [babashka.fs :as fs]
            [clojure.edn :as edn]
            [clojure.string :as str]
            [clojure.test :refer [deftest is testing]]
            [release :as r]))

(defn- bytes-of
  "A byte array of size n (zero filled) with the given [offset [b ...]] runs."
  [n & runs]
  (let [bs (byte-array n)]
    (doseq [[offset values] (partition 2 runs)
            [i b] (map-indexed vector values)]
      (aset-byte bs (+ offset i) (unchecked-byte b)))
    bs))

(def macho-arm64  (bytes-of 64 0 [0xCF 0xFA 0xED 0xFE 0x0C 0x00 0x00 0x01]))
(def macho-x86_64 (bytes-of 64 0 [0xCF 0xFA 0xED 0xFE 0x07 0x00 0x00 0x01]))
(def macho-fat    (bytes-of 64 0 [0xCA 0xFE 0xBA 0xBE 0 0 0 2]
                            8 [0x01 0x00 0x00 0x07] 28 [0x01 0x00 0x00 0x0C]))
(def elf-x86_64   (bytes-of 64 0 [0x7F 0x45 0x4C 0x46 2 1] 18 [0x3E 0x00]))
(def elf-aarch64  (bytes-of 64 0 [0x7F 0x45 0x4C 0x46 2 1] 18 [0xB7 0x00]))
(def pe-x86_64    (bytes-of 128 0 [0x4D 0x5A] 0x3C [0x40 0 0 0] 0x40 [0x50 0x45 0 0 0x64 0x86]))

(deftest reads-headers
  (is (= {:os "macos" :cpus #{"aarch64"}} (r/header macho-arm64)))
  (is (= {:os "macos" :cpus #{"x86_64"}} (r/header macho-x86_64)))
  (is (= {:os "macos" :cpus #{"x86_64" "aarch64"}} (r/header macho-fat)))
  (is (= {:os "linux" :cpus #{"x86_64"}} (r/header elf-x86_64)))
  (is (= {:os "linux" :cpus #{"aarch64"}} (r/header elf-aarch64)))
  (is (= {:os "windows" :cpus #{"x86_64"}} (r/header pe-x86_64)))
  (testing "not executables"
    (is (nil? (r/header (.getBytes "{:not \"a binary\"}"))))
    (is (nil? (r/header (byte-array 3))))
    (is (nil? (r/header (bytes-of 128 0 [0x4D 0x5A])))) "MZ without a PE header"))

(deftest checks-headers-against-platforms
  (let [mac-arm (r/platform-for-target "aarch64-apple-darwin")
        linux   (r/platform-for-target "x86_64-unknown-linux-gnu")]
    (is (nil? (r/header-problem (r/header macho-arm64) mac-arm)))
    (is (nil? (r/header-problem (r/header macho-fat) mac-arm)) "a universal binary covers it")
    (is (= "built for x86_64, listed as aarch64"
           (r/header-problem (r/header macho-x86_64) mac-arm))
        "fswatcher 0.0.5")
    (is (= "built for macos, listed as linux" (r/header-problem (r/header macho-x86_64) linux)))
    (is (= "not a Mach-O, ELF, or PE executable" (r/header-problem nil linux)))))

(deftest maps-targets-to-registry-platforms
  (is (= ["Linux.*" "amd64" "linux-amd64"]
         ((juxt :os/name :os/arch :slug) (r/platform-for-target "x86_64-unknown-linux-gnu"))))
  (is (= ["Mac.*" "x86_64"]
         ((juxt :os/name :os/arch) (r/platform-for-target "x86_64-apple-darwin"))))
  (is (= "windows-amd64" (:slug (r/platform-for-target "x86_64-pc-windows-gnu"))))
  (is (thrown? clojure.lang.ExceptionInfo (r/platform-for-target "riscv64gc-unknown-linux-gnu"))))

(defn- fake-zip [dist slug exe header-bytes]
  (let [staging (fs/create-temp-dir)
        bin     (fs/path staging exe)]
    (fs/write-bytes bin header-bytes)
    (fs/zip (fs/path dist (str "pod-sqlite-9.9.9-" slug ".zip")) [(str bin)] {:root (str staging)})
    (fs/delete-tree staging)))

(defn- run-manifest [dist]
  (with-out-str
    (r/manifest {:pod "sqlite" :version "9.9.9" :dist (str dist)
                 :base-url "https://example.com/releases/download/pod-sqlite-v9.9.9/"
                 :name "joshrotenberg/pod-sqlite" :description "test"})))

(deftest manifest-from-checked-zips
  (let [dist (fs/create-temp-dir)]
    (fake-zip dist "macos-aarch64" "pod-sqlite" macho-arm64)
    (fake-zip dist "macos-x86_64" "pod-sqlite" macho-x86_64)
    (fake-zip dist "linux-amd64" "pod-sqlite" elf-x86_64)
    (fake-zip dist "linux-aarch64" "pod-sqlite" elf-aarch64)
    (fake-zip dist "windows-amd64" "pod-sqlite.exe" pe-x86_64)
    (run-manifest dist)
    (let [m (edn/read-string (slurp (str (fs/path dist "manifest.edn"))))]
      (is (= 'joshrotenberg/pod-sqlite (:pod/name m)))
      (is (= "9.9.9" (:pod/version m)))
      (is (= [["Linux.*" "amd64"] ["Linux.*" "aarch64"] ["Mac.*" "aarch64"]
              ["Mac.*" "x86_64"] ["Windows.*" "amd64"]]
             (map (juxt :os/name :os/arch) (:pod/artifacts m))))
      (is (= "https://example.com/releases/download/pod-sqlite-v9.9.9/pod-sqlite-9.9.9-macos-aarch64.zip"
             (:artifact/url (nth (:pod/artifacts m) 2))))
      (is (= "pod-sqlite.exe" (:artifact/executable (last (:pod/artifacts m))))))
    (fs/delete-tree dist)))

(deftest manifest-refuses-a-mislabeled-binary
  (testing "an x86_64 binary filed as macos-aarch64, as in fswatcher 0.0.5"
    (let [dist (fs/create-temp-dir)]
      (fake-zip dist "macos-aarch64" "pod-sqlite" macho-x86_64)
      (fake-zip dist "linux-amd64" "pod-sqlite" elf-x86_64)
      (let [e (try (run-manifest dist) nil
                   (catch clojure.lang.ExceptionInfo e e))]
        (is (some? e))
        (is (str/includes? (ex-message e) "1 artifact(s) do not match")))
      (is (not (fs/exists? (fs/path dist "manifest.edn"))))
      (fs/delete-tree dist))))

(deftest reads-the-crate-version
  (is (re-matches #"\d+\.\d+\.\d+.*" (r/crate-version "sqlite"))))
