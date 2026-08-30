// Le cycle de vie d'un client officiel, exerce par `go-elasticsearch` 8.x.
//
// Lance dans un conteneur par `tests/compat/tests_clients.py`, contre l'URL
// passee en argument. N'importe que le client officiel tire de son registre :
// le code ci-dessous est ecrit ici, la bibliotheque qu'il exerce ne l'est pas.
//
// Meme format de sortie que les deux autres batteries :
// `CAS <nom> <PASS|FAIL> <detail>`.
package main

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"io"
	"os"
	"strings"
	"time"

	"github.com/elastic/go-elasticsearch/v8"
	"github.com/elastic/go-elasticsearch/v8/esapi"
	"github.com/elastic/go-elasticsearch/v8/esutil"
)

const index = "cycle-go"

type cas struct {
	nom string
	f   func(*elasticsearch.Client, string) (string, error)
}

var batterie []cas

func ajoute(nom string, f func(*elasticsearch.Client, string) (string, error)) {
	batterie = append(batterie, cas{nom, f})
}

// corps lit et referme une reponse, et rend son JSON.
func corps(res *esapi.Response) (map[string]any, error) {
	defer res.Body.Close()
	brut, err := io.ReadAll(res.Body)
	if err != nil {
		return nil, err
	}
	var doc map[string]any
	if err := json.Unmarshal(brut, &doc); err != nil {
		return nil, fmt.Errorf("corps illisible (%d) : %s", res.StatusCode, string(brut[:min(200, len(brut))]))
	}
	return doc, nil
}

func min(a, b int) int {
	if a < b {
		return a
	}
	return b
}

func init() {
	// -----------------------------------------------------------------------
	// 1. Ce que le client fait avant qu'on lui demande quoi que ce soit
	// -----------------------------------------------------------------------

	ajoute("decouverte_version", func(es *elasticsearch.Client, _ string) (string, error) {
		res, err := es.Info()
		if err != nil {
			return "", err
		}
		doc, err := corps(res)
		if err != nil {
			return "", err
		}
		version := doc["version"].(map[string]any)["number"].(string)
		if !strings.HasPrefix(version, "8.") {
			return "", fmt.Errorf("version %s, le client 8.x exige une 8", version)
		}
		if doc["tagline"] != "You Know, for Search" {
			return "", fmt.Errorf("tagline %v", doc["tagline"])
		}
		return version, nil
	})

	ajoute("entete_produit", func(es *elasticsearch.Client, _ string) (string, error) {
		// Le client go verifie lui-meme `X-Elastic-Product` sur la premiere
		// reponse et refuse de continuer s'il ne le trouve pas. Le cas le relit
		// sur trois formes de reponse, parce qu'ES le pose sur toutes.
		vus := map[string]string{}
		res, err := es.Info()
		if err != nil {
			return "", err
		}
		vus["/"] = res.Header.Get("X-Elastic-Product")
		res.Body.Close()

		res, err = es.Search(es.Search.WithIndex("*"), es.Search.WithSize(0))
		if err != nil {
			return "", err
		}
		vus["_search"] = res.Header.Get("X-Elastic-Product")
		res.Body.Close()

		res, err = es.Bulk(strings.NewReader(
			fmt.Sprintf("{\"index\":{\"_index\":%q,\"_id\":\"entete\"}}\n{\"n\":1}\n", index)),
			es.Bulk.WithRefresh("true"))
		if err != nil {
			return "", err
		}
		vus["_bulk"] = res.Header.Get("X-Elastic-Product")
		res.Body.Close()

		for route, valeur := range vus {
			if valeur != "Elasticsearch" {
				return "", fmt.Errorf("en-tete [%s] sur %s, attendu [Elasticsearch]", valeur, route)
			}
		}
		return "Elasticsearch sur /, _search, _bulk", nil
	})

	ajoute("negociation_compression", func(_ *elasticsearch.Client, url string) (string, error) {
		// `CompressRequestBody` gzippe le corps envoye. Un serveur qui ne sait
		// pas le lire rend un 400 sur un JSON qu'il juge invalide.
		gz, err := elasticsearch.NewClient(elasticsearch.Config{
			Addresses:           []string{url},
			CompressRequestBody: true,
		})
		if err != nil {
			return "", err
		}
		var lot bytes.Buffer
		for i := 0; i < 200; i++ {
			fmt.Fprintf(&lot, "{\"index\":{\"_index\":%q,\"_id\":\"gz%d\"}}\n", index, i)
			fmt.Fprintf(&lot, "{\"texte\":%q}\n", strings.Repeat(fmt.Sprintf("document %d ", i), 40))
		}
		res, err := gz.Bulk(bytes.NewReader(lot.Bytes()), gz.Bulk.WithRefresh("true"))
		if err != nil {
			return "", err
		}
		doc, err := corps(res)
		if err != nil {
			return "", err
		}
		if res.IsError() || doc["errors"] == true {
			return "", fmt.Errorf("lot gzippe refuse (%d) : %v", res.StatusCode, doc["error"])
		}
		return "200 documents envoyes en gzip", nil
	})

	ajoute("sniffing", func(es *elasticsearch.Client, _ string) (string, error) {
		// `DiscoverNodes` demande `GET /_nodes/http` et remplace le pool de
		// connexions par ce qu'il rend. Le tenir ou le refuser sont deux
		// reponses acceptables ; se taire n'en est pas une.
		if err := es.DiscoverNodes(); err != nil {
			if strings.TrimSpace(err.Error()) == "" {
				return "", fmt.Errorf("refus muet : erreur sans message")
			}
			return fmt.Sprintf("refuse proprement (%.120s)", err.Error()), nil
		}
		res, err := es.Info()
		if err != nil {
			return "", fmt.Errorf("sniffing accepte mais le client a perdu la main : %w", err)
		}
		res.Body.Close()
		return "sniffing tenu, le client a garde la main", nil
	})

	// -----------------------------------------------------------------------
	// 2. Ce que le client fait des erreurs
	// -----------------------------------------------------------------------

	ajoute("erreurs_typees", func(es *elasticsearch.Client, _ string) (string, error) {
		var vus []string

		res, err := es.Get(index, "jamais-ecrit")
		if err != nil {
			return "", err
		}
		doc, err := corps(res)
		if err != nil {
			return "", err
		}
		if res.StatusCode != 404 || doc["found"] != false {
			return "", fmt.Errorf("document absent : statut %d, corps %v", res.StatusCode, doc)
		}
		vus = append(vus, "404 found:false")

		res, err = es.Search(es.Search.WithIndex(index),
			es.Search.WithBody(strings.NewReader(`{"query":{"pas_une_clause":{}}}`)))
		if err != nil {
			return "", err
		}
		doc, err = corps(res)
		if err != nil {
			return "", err
		}
		if res.StatusCode != 400 || !res.IsError() {
			return "", fmt.Errorf("clause inconnue : statut %d", res.StatusCode)
		}
		erreur, ok := doc["error"].(map[string]any)
		if !ok || erreur["type"] == nil || erreur["reason"] == nil {
			return "", fmt.Errorf("erreur hors format ES : %v", doc)
		}
		vus = append(vus, fmt.Sprintf("400 %v", erreur["type"]))

		res, err = es.Index(index, strings.NewReader(`{"n":1}`),
			es.Index.WithDocumentID("conflit"), es.Index.WithRefresh("true"))
		if err != nil {
			return "", err
		}
		res.Body.Close()
		res, err = es.Index(index, strings.NewReader(`{"n":2}`),
			es.Index.WithDocumentID("conflit"),
			es.Index.WithIfSeqNo(99999), es.Index.WithIfPrimaryTerm(1))
		if err != nil {
			return "", err
		}
		doc, err = corps(res)
		if err != nil {
			return "", err
		}
		if res.StatusCode != 409 {
			return "", fmt.Errorf("`if_seq_no` perime : statut %d, corps %v", res.StatusCode, doc)
		}
		erreur, _ = doc["error"].(map[string]any)
		if erreur["type"] != "version_conflict_engine_exception" {
			return "", fmt.Errorf("type d'erreur %v", erreur["type"])
		}
		vus = append(vus, "409 version_conflict_engine_exception")

		return strings.Join(vus, ", "), nil
	})

	// -----------------------------------------------------------------------
	// 3. Les helpers
	// -----------------------------------------------------------------------

	ajoute("helpers_bulk_indexer", func(es *elasticsearch.Client, _ string) (string, error) {
		// `esutil.BulkIndexer` est le helper d'import du client go : il
		// decoupe, envoie sur plusieurs fils, et compte. Ses compteurs sont la
		// mesure — un import qui perd des documents en silence serait le pire
		// resultat possible.
		bi, err := esutil.NewBulkIndexer(esutil.BulkIndexerConfig{
			Client:     es,
			Index:      index,
			NumWorkers: 4,
			FlushBytes: 200000,
			Refresh:    "true",
		})
		if err != nil {
			return "", err
		}
		ctx := context.Background()
		var refuses int
		for i := 0; i < 1500; i++ {
			err := bi.Add(ctx, esutil.BulkIndexerItem{
				Action:     "index",
				DocumentID: fmt.Sprintf("b%d", i),
				Body:       strings.NewReader(fmt.Sprintf(`{"rang":%d,"flux":true}`, i)),
				OnFailure: func(_ context.Context, _ esutil.BulkIndexerItem, _ esutil.BulkIndexerResponseItem, _ error) {
					refuses++
				},
			})
			if err != nil {
				return "", err
			}
		}
		if err := bi.Close(ctx); err != nil {
			return "", err
		}
		stats := bi.Stats()
		if stats.NumFailed != 0 || refuses != 0 {
			return "", fmt.Errorf("%d echecs (%d rappels OnFailure)", stats.NumFailed, refuses)
		}
		if stats.NumIndexed != 1500 {
			return "", fmt.Errorf("%d documents indexes", stats.NumIndexed)
		}
		res, err := es.Count(es.Count.WithIndex(index),
			es.Count.WithBody(strings.NewReader(`{"query":{"term":{"flux":true}}}`)))
		if err != nil {
			return "", err
		}
		doc, err := corps(res)
		if err != nil {
			return "", err
		}
		if compte, _ := doc["count"].(float64); compte != 1500 {
			return "", fmt.Errorf("%v documents relus", doc["count"])
		}
		return fmt.Sprintf("1500 documents en %d requetes, 0 echec", stats.NumRequests), nil
	})

	ajoute("scroll_deroule", func(es *elasticsearch.Client, _ string) (string, error) {
		// Le client go n'a pas de helper `scan` : l'export s'ecrit avec
		// `Search(WithScroll)` puis `Scroll`. C'est ce code-la qu'il faut
		// exercer, pas l'idee qu'on s'en fait.
		res, err := es.Search(
			es.Search.WithIndex(index),
			es.Search.WithBody(strings.NewReader(`{"query":{"term":{"flux":true}}}`)),
			es.Search.WithSize(137),
			es.Search.WithScroll(time.Minute),
		)
		if err != nil {
			return "", err
		}
		doc, err := corps(res)
		if err != nil {
			return "", err
		}
		vus := map[string]bool{}
		pages := 0
		for {
			id, _ := doc["_scroll_id"].(string)
			hits := doc["hits"].(map[string]any)["hits"].([]any)
			if len(hits) == 0 {
				if id != "" {
					r, _ := es.ClearScroll(es.ClearScroll.WithScrollID(id))
					if r != nil {
						r.Body.Close()
					}
				}
				break
			}
			pages++
			for _, h := range hits {
				vus[h.(map[string]any)["_id"].(string)] = true
			}
			res, err = es.Scroll(es.Scroll.WithScrollID(id), es.Scroll.WithScroll(time.Minute))
			if err != nil {
				return "", err
			}
			doc, err = corps(res)
			if err != nil {
				return "", err
			}
		}
		if len(vus) != 1500 {
			return "", fmt.Errorf("%d documents distincts sur 1500 en %d pages", len(vus), pages)
		}
		return fmt.Sprintf("1500 documents deroules en %d pages, sans doublon", pages), nil
	})
}

func main() {
	url := "http://localhost:9200"
	if len(os.Args) > 1 {
		url = os.Args[1]
	}
	es, err := elasticsearch.NewClient(elasticsearch.Config{Addresses: []string{url}})
	if err != nil {
		fmt.Printf("CAS demarrage FAIL %s\n", err)
		fmt.Printf("CYCLE go 0/%d\n", len(batterie))
		os.Exit(1)
	}

	if res, err := es.Indices.Delete([]string{index},
		es.Indices.Delete.WithIgnoreUnavailable(true)); err == nil {
		res.Body.Close()
	}
	res, err := es.Indices.Create(index, es.Indices.Create.WithBody(strings.NewReader(
		`{"mappings":{"properties":{"rang":{"type":"integer"},"flux":{"type":"boolean"},`+
			`"texte":{"type":"text"},"n":{"type":"integer"}}}}`)))
	if err != nil {
		fmt.Printf("CAS demarrage FAIL creation de l'index : %s\n", err)
		fmt.Printf("CYCLE go 0/%d\n", len(batterie))
		os.Exit(1)
	}
	res.Body.Close()

	rates := 0
	for _, c := range batterie {
		detail, err := c.f(es, url)
		if err != nil {
			fmt.Printf("CAS %s FAIL %s\n", c.nom, strings.SplitN(err.Error(), "\n", 2)[0])
			rates++
			continue
		}
		fmt.Printf("CAS %s PASS %s\n", c.nom, detail)
	}
	if res, err := es.Indices.Delete([]string{index},
		es.Indices.Delete.WithIgnoreUnavailable(true)); err == nil {
		res.Body.Close()
	}
	fmt.Printf("CYCLE go %d/%d\n", len(batterie)-rates, len(batterie))
	if rates > 0 {
		os.Exit(1)
	}
}
