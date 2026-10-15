#!/usr/bin/env bb
;; Exercises the fswatcher pod. Run from the repository root:
;;
;;   cargo build -p pod-fswatcher
;;   bb pods/fswatcher/test.bb [socket]
;;
;; Prints one "name: value" line per check; tests/bb.rs asserts on them.
;; Leaves one watcher running on purpose, so exiting also tests shutdown.

(require '[babashka.pods :as pods]
         '[babashka.fs :as fs]
         '[clojure.string :as str])

(def transport (if (= "socket" (first *command-line-args*)) :socket :stdio))
(def pod-path (or (System/getenv "FSWATCHER_POD") "target/debug/pod-fswatcher"))
(pods/load-pod [pod-path] (cond-> {} (= :socket transport) (assoc :transport :socket)))
(require '[pod.babashka.fswatcher :as fw])

(defn check [label v] (println (str label ":") (pr-str v)))

(println "transport:" transport)
;; tests/bb.rs checks that these processes are gone once bb has exited.
(println "pod pids:" (str/join " " (map #(.pid %) (.toArray (.children (java.lang.ProcessHandle/current))))))

(def tmp (str (fs/create-temp-dir {:prefix "pod-fswatcher-test"})))
(defn rel [event] (update event :path #(str/replace % tmp "<dir>")))

(def deduped (atom []))
(def all-events (atom []))

(def a-txt (promise))
;; FSEvents can also report the temp dir's own creation, which happened just
;; before the watch started, so wait for a.txt specifically.
(def w1 (fw/watch tmp (fn [e]
                        (let [e (rel e)]
                          (swap! deduped conj e)
                          (when (= "<dir>/a.txt" (:path e))
                            (deliver a-txt e))))
                  {:delay-ms 50}))
(check "watcher" w1)
;; Undeduplicated and never unwatched.
(def w2 (fw/watch tmp (fn [e] (swap! all-events conj (rel e))) {:dedup false :recursive true}))
(check "second watcher" w2)

;; FSEvents starts delivering shortly after the stream is created.
(Thread/sleep 300)
(spit (str tmp "/a.txt") "hello")
(check "event" (deref a-txt 5000 :timeout))

(check "unwatch" (fw/unwatch w1))
(check "unwatch again" (fw/unwatch w1))
(spit (str tmp "/b.txt") "after unwatch")
(Thread/sleep 500)
(check "deduped a.txt events" (filterv #(= "<dir>/a.txt" (:path %)) @deduped))
(check "events after unwatch" (filterv #(= "<dir>/b.txt" (:path %)) @deduped))
(check "second watcher saw b.txt" (boolean (some #(= "<dir>/b.txt" (:path %)) @all-events)))

(check "missing path" (try (fw/watch (str tmp "/nope") prn)
                           (catch clojure.lang.ExceptionInfo e
                             (str/replace (ex-message e) tmp "<dir>"))))
(check "bad unwatch" (try (fw/unwatch 42)
                          (catch clojure.lang.ExceptionInfo e (ex-message e))))
(check "doc" (first (str/split-lines (:doc (meta #'fw/watch)))))
(check "raw a.txt types" (mapv :type (filter #(= "<dir>/a.txt" (:path %)) @all-events)))
(fs/delete-tree tmp)
(println "done")
