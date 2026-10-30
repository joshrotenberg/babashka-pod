(require '[babashka.pods :as pods])
(pods/load-pod [(System/getenv "AGENT_POD")])
(require '[pod.joshrotenberg.agent :as agent])

(def provider (keyword (or (first *command-line-args*) "claude")))
(def model (second *command-line-args*))
(defn t [] (System/currentTimeMillis))
(defn brief [r] (select-keys r [:status :text :data :session-id :cost-usd :duration-ms :error]))

(def a (agent/create (cond-> {:provider provider :cwd (System/getProperty "user.dir")}
                       model (assoc :model model))))

(let [t0 (t) r (agent/ask a "Reply with exactly the word: apple")]
  (println "ask 1:" (pr-str (brief r)) "wall-ms" (- (t) t0)))
(println "session-id:" (agent/session-id a))
(let [r (agent/ask a "What word did you reply with last time? Reply with just that word.")]
  (println "ask 2 (resumed):" (pr-str (brief r))))

(let [turn (agent/tell a "Count from 1 to 5, one number per line.")
      polls (loop [n 0] (if (= :running (agent/status turn)) (do (Thread/sleep 100) (recur (inc n))) n))
      seen (atom [])]
  (agent/events turn #(swap! seen conj (or (get % "type") (:type %))))
  (println "tell: polled" polls "times; status" (agent/status turn))
  (println "tell result:" (pr-str (brief (agent/result turn))))
  (println "events replayed:" (count @seen) (pr-str (frequencies @seen))))

(let [r (agent/ask a "Give the capital of France."
                   {:output-schema {:type "object"
                                    :properties {:city {:type "string"}}
                                    :required ["city"]
                                    :additionalProperties false}})]
  (println "schema:" (pr-str (select-keys r [:status :data :error]))))

(let [turn (agent/tell a "Write a 2000 word essay about rivers.")]
  (Thread/sleep 3000)
  (agent/cancel turn)
  (let [t0 (t) r (agent/result turn 20000)]
    (println "cancel:" (pr-str (select-keys r [:status :error])) "settled-ms" (- (t) t0))))

(let [r (agent/ask a "Write a 2000 word essay about mountains." {:timeout-ms 3000})]
  (println "timeout:" (pr-str (select-keys r [:status :error]))))

(try (let [turn (agent/tell a "Say hi.")]
       (agent/tell a "Say hi again.")
       (agent/cancel turn))
     (catch Exception e (println "busy:" (ex-message e))))
(agent/close a)
(println "done")
