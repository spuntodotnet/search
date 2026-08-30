/*
 * Le cycle de vie d'un client officiel, exerce par `@elastic/elasticsearch` 8.x.
 *
 * Lance dans un conteneur par `tests/compat/tests_clients.py`, contre l'URL
 * passee en argument. N'importe que le client officiel installe depuis npm :
 * le code ci-dessous est ecrit ici, la bibliotheque qu'il exerce ne l'est pas.
 *
 * Meme format de sortie que les deux autres batteries :
 * `CAS <nom> <PASS|FAIL> <detail>`.
 */
'use strict'

const { Client, errors, events } = require('@elastic/elasticsearch')

const URL = process.argv[2] || 'http://localhost:9200'
const INDEX = 'cycle-javascript'
const CAS = []

function cas (nom, f) { CAS.push([nom, f]) }
function exige (condition, message) { if (!condition) throw new Error(message) }

// ---------------------------------------------------------------------------
// 1. Ce que le client fait avant qu'on lui demande quoi que ce soit
// ---------------------------------------------------------------------------

cas('decouverte_version', async (es) => {
  const info = await es.info()
  const numero = info.version.number
  exige(numero.split('.')[0] === '8', `version majeure ${numero}, le client 8.x exige 8`)
  exige(info.tagline === 'You Know, for Search', info.tagline)
  exige(Boolean(info.cluster_name), 'cluster_name vide')
  return numero
})

cas('entete_produit', async (es) => {
  // Sans `x-elastic-product`, le client 8.x leve `ProductNotSupportedError`
  // et aucun appel ne passe. Verifie sur trois formes de reponse.
  const vus = {}
  vus['/'] = (await es.info({}, { meta: true })).headers['x-elastic-product']
  vus._search = (await es.search({ index: '*', size: 0 }, { meta: true })).headers['x-elastic-product']
  vus._bulk = (await es.bulk({
    operations: [{ index: { _index: INDEX, _id: 'entete' } }, { n: 1 }],
    refresh: true
  }, { meta: true })).headers['x-elastic-product']
  const manquants = Object.keys(vus).filter((k) => vus[k] !== 'Elasticsearch')
  exige(manquants.length === 0, `en-tete absent ou faux sur ${manquants} (${JSON.stringify(vus)})`)
  return 'Elasticsearch sur ' + Object.keys(vus).join(', ')
})

cas('negociation_compression', async (_es, url) => {
  // `compression: true` gzippe le corps de la requete. Un serveur qui ne sait
  // pas le lire rend un 400 sur un JSON qu'il juge invalide.
  const gz = new Client({ node: url, compression: true })
  try {
    const lignes = []
    for (let i = 0; i < 200; i++) {
      lignes.push({ index: { _index: INDEX, _id: `gz${i}` } })
      lignes.push({ texte: `document ${i} `.repeat(40) })
    }
    const rep = await gz.bulk({ operations: lignes, refresh: true })
    exige(rep.errors === false, JSON.stringify(rep.items && rep.items[0]))
    const trouves = await gz.search({ index: INDEX, query: { match: { texte: 'document' } }, size: 0 })
    exige(trouves.hits.total.value >= 200, JSON.stringify(trouves.hits.total))
    return '200 documents envoyes en gzip, relus'
  } finally {
    await gz.close()
  }
})

cas('sniffing', async (_es, url) => {
  // `sniffOnStart` demande `GET /_nodes/_all/http` et remplace le pool par ce
  // qu'il rend. Le client JavaScript ne leve pas si le sniff echoue : il emet
  // un evenement. Le lire est donc la seule facon de savoir laquelle des deux
  // reponses on a eue — sans lui, un refus se lirait « sniffing tenu ».
  const sniff = new Client({ node: url, sniffOnStart: true, sniffEndpoint: '_nodes/_all/http' })
  let echec = null
  let vu = false
  sniff.diagnostic.on(events.SNIFF, (err) => { vu = true; echec = err })
  try {
    await new Promise((resolve) => setTimeout(resolve, 500))
    const info = await sniff.info()
    exige(Boolean(info.version.number), JSON.stringify(info))
    exige(vu, "le client n'a pas sniffe du tout")
    if (echec == null) return 'sniffing tenu, le client a garde la main'
    exige(String(echec.message || '').trim().length > 0, `${echec.name} sans message : un refus muet`)
    return `refuse proprement, client toujours utilisable (${echec.name}: ${String(echec.message).slice(0, 100)})`
  } finally {
    await sniff.close()
  }
})

// ---------------------------------------------------------------------------
// 2. Ce que le client fait des erreurs
// ---------------------------------------------------------------------------

cas('erreurs_typees', async (es) => {
  const vus = []

  try {
    await es.get({ index: INDEX, id: 'jamais-ecrit' })
    throw new Error('un document absent aurait du lever ResponseError')
  } catch (e) {
    exige(e instanceof errors.ResponseError, `${e.name} au lieu de ResponseError`)
    exige(e.statusCode === 404, `statut ${e.statusCode}`)
    vus.push('ResponseError(404)')
  }

  try {
    await es.search({ index: INDEX, query: { pas_une_clause: {} } })
    throw new Error('une clause inconnue aurait du lever ResponseError')
  } catch (e) {
    exige(e instanceof errors.ResponseError, `${e.name} au lieu de ResponseError`)
    exige(e.statusCode === 400, `statut ${e.statusCode}`)
    exige(Boolean(e.body.error.type), JSON.stringify(e.body))
    exige(Boolean(e.body.error.reason), JSON.stringify(e.body))
    vus.push(`ResponseError(400, ${e.body.error.type})`)
  }

  await es.index({ index: INDEX, id: 'conflit', document: { n: 1 }, refresh: true })
  try {
    await es.index({ index: INDEX, id: 'conflit', document: { n: 2 }, if_seq_no: 99999, if_primary_term: 1 })
    throw new Error('un `if_seq_no` perime aurait du lever')
  } catch (e) {
    exige(e.statusCode === 409, `statut ${e.statusCode}`)
    exige(e.body.error.type === 'version_conflict_engine_exception', JSON.stringify(e.body))
    vus.push('ResponseError(409, version_conflict_engine_exception)')
  }

  // L'option `ignore` transforme un statut attendu en reponse normale : c'est
  // ce que fait tout code qui teste l'existence d'un document.
  const absent = await es.get({ index: INDEX, id: 'toujours-pas' }, { ignore: [404] })
  exige(absent.found === false, JSON.stringify(absent))
  vus.push('ignore:[404] rend le corps')

  return vus.join(', ')
})

// ---------------------------------------------------------------------------
// 3. Les helpers
// ---------------------------------------------------------------------------

cas('helpers_bulk', async (es) => {
  // Le helper decoupe, envoie, retente, et compte. `onDrop` est appele pour
  // chaque document refuse : un helper qui avalerait un rejet rendrait un
  // index incomplet en silence.
  const documents = []
  for (let i = 0; i < 1500; i++) documents.push({ rang: i, flux: true })
  const jetes = []
  const resultat = await es.helpers.bulk({
    datasource: documents,
    flushBytes: 200000,
    refreshOnCompletion: INDEX,
    onDrop (doc) { jetes.push(doc) },
    onDocument (doc) { return { index: { _index: INDEX, _id: `b${doc.rang}` } } }
  })
  exige(jetes.length === 0, `${jetes.length} documents jetes`)
  exige(resultat.successful === 1500, `${resultat.successful} indexes`)
  exige(resultat.failed === 0, `${resultat.failed} echecs`)
  const compte = await es.count({ index: INDEX, query: { term: { flux: true } } })
  exige(compte.count === 1500, `${compte.count} documents relus`)
  return `1500 documents indexes en ${resultat.total} operations, 0 jete`
})

cas('helpers_scroll', async (es) => {
  // `scrollSearch` deroule un `scroll` page par page et ferme le contexte.
  // Chaque document doit sortir une fois et une seule.
  const vus = new Set()
  let pages = 0
  for await (const reponse of es.helpers.scrollSearch({
    index: INDEX,
    query: { term: { flux: true } },
    size: 137
  })) {
    pages += 1
    for (const hit of reponse.documents) vus.add(hit.rang)
  }
  exige(vus.size === 1500, `${vus.size} documents distincts sur 1500, en ${pages} pages`)
  return `1500 documents deroules en ${pages} pages, sans doublon`
})

// ---------------------------------------------------------------------------

async function main () {
  const es = new Client({ node: URL })
  if (await es.indices.exists({ index: INDEX })) {
    await es.indices.delete({ index: INDEX })
  }
  await es.indices.create({
    index: INDEX,
    mappings: {
      properties: {
        rang: { type: 'integer' },
        flux: { type: 'boolean' },
        texte: { type: 'text' },
        n: { type: 'integer' }
      }
    }
  })
  let rates = 0
  for (const [nom, f] of CAS) {
    try {
      const detail = await f(es, URL)
      console.log(`CAS ${nom} PASS ${detail}`)
    } catch (e) {
      console.log(`CAS ${nom} FAIL ${e.name}: ${String(e.message).split('\n')[0]}`)
      for (const ligne of String(e.stack || e).split('\n')) console.log(`    | ${ligne}`)
      rates += 1
    }
  }
  await es.indices.delete({ index: INDEX }, { ignore: [404] })
  await es.close()
  console.log(`CYCLE javascript ${CAS.length - rates}/${CAS.length}`)
  process.exit(rates ? 1 : 0)
}

main().catch((e) => {
  console.log(`CAS demarrage FAIL ${e.name}: ${String(e.message).split('\n')[0]}`)
  console.log(`CYCLE javascript 0/${CAS.length}`)
  process.exit(1)
})
