// Le module n'existe que pour tirer le client officiel depuis son registre :
// c'est bien la bibliotheque publiee qui est exercee, pas une copie.
module ferrite/cycle

go 1.21

require github.com/elastic/go-elasticsearch/v8 v8.15.0
