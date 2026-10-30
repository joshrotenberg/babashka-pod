(require '[babashka.pods :as pods])
(pods/load-pod [(System/getenv "AGENT_POD")])
(require '[pod.joshrotenberg.agent :as agent])
(def mode (first *command-line-args*))
(def a (agent/create {:provider (keyword (second *command-line-args*)) :cwd (System/getProperty "user.dir")
                      :model (nth *command-line-args* 2 nil)}))
(case mode
  "tell-then-exit" (do (agent/tell a "Write a 3000 word essay about glaciers, marker ZQXJ") (Thread/sleep 2500) (println "exiting with turn running"))
  "ask-block" (do (println "asking") (flush) (agent/ask a "Write a 3000 word essay about deserts, marker ZQXJ")))
