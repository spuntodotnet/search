//! Les listes de mots vides des analyzers de langue d'Elasticsearch.
//!
//! Genere par `tests/compat/releve_mots_vides.py --toutes`, qui les lit dans le
//! `lucene-analysis-common-*.jar` du **conteneur de reference** — le fichier
//! qu'Elasticsearch ouvre lui-meme — puis les **verifie** contre lui, mot a mot
//! (chaque mot de la liste doit ne rendre aucun token) et a l'envers (sur le
//! vocabulaire complet de la langue, aucun mot hors liste ne doit disparaitre).
//!
//! Ne pas editer a la main : la liste francaise l'avait ete, et il y manquait
//! `cela` sous sa graphie ancienne (`cela` avec accent grave) — un mot qu'ES
//! ecarte et que ferrite indexait, en silence.
//!
//! Une liste tient dans une seule chaine, separee par des sauts de ligne : 15
//! tableaux de `&str` couteraient une table de pointeurs de plusieurs dizaines
//! de kilo-octets dans un binaire qui en pese 4 M.

/// `_danish_` — 94 mots.
pub const DANISH: &str = "\
og\ni\njeg\ndet\nat\nen\nden\ntil\ner\nsom\npå\nde\nmed\nhan\naf\nfor\nikke\nder\nvar\nmig\n\
sig\nmen\net\nhar\nom\nvi\nmin\nhavde\nham\nhun\nnu\nover\nda\nfra\ndu\nud\nsin\ndem\nos\n\
op\nman\nhans\nhvor\neller\nhvad\nskal\nselv\nher\nalle\nvil\nblev\nkunne\nind\nnår\nvære\n\
dog\nnoget\nville\njo\nderes\nefter\nned\nskulle\ndenne\nend\ndette\nmit\nogså\nunder\nhave\n\
dig\nanden\nhende\nmine\nalt\nmeget\nsit\nsine\nvor\nmod\ndisse\nhvis\ndin\nnogle\nhos\n\
blive\nmange\nad\nbliver\nhendes\nværet\nthi\njer\nsådan\n";

/// `_dutch_` — 101 mots.
pub const DUTCH: &str = "\
de\nen\nvan\nik\nte\ndat\ndie\nin\neen\nhij\nhet\nniet\nzijn\nis\nwas\nop\naan\nmet\nals\n\
voor\nhad\ner\nmaar\nom\nhem\ndan\nzou\nof\nwat\nmijn\nmen\ndit\nzo\ndoor\nover\nze\nzich\n\
bij\nook\ntot\nje\nmij\nuit\nder\ndaar\nhaar\nnaar\nheb\nhoe\nheeft\nhebben\ndeze\nu\nwant\n\
nog\nzal\nme\nzij\nnu\nge\ngeen\nomdat\niets\nworden\ntoch\nal\nwaren\nveel\nmeer\ndoen\n\
toen\nmoet\nben\nzonder\nkan\nhun\ndus\nalles\nonder\nja\neens\nhier\nwie\nwerd\naltijd\n\
doch\nwordt\nwezen\nkunnen\nons\nzelf\ntegen\nna\nreeds\nwil\nkon\nniets\nuw\niemand\n\
geweest\nandere\n";

/// `_english_` — 33 mots.
pub const ENGLISH: &str = "\
a\nan\nand\nare\nas\nat\nbe\nbut\nby\nfor\nif\nin\ninto\nis\nit\nno\nnot\nof\non\nor\nsuch\n\
that\nthe\ntheir\nthen\nthere\nthese\nthey\nthis\nto\nwas\nwill\nwith\n";

/// `_finnish_` — 229 mots.
pub const FINNISH: &str = "\
olla\nolen\nolet\non\nolemme\nolette\novat\nole\noli\nolisi\nolisit\nolisin\nolisimme\n\
olisitte\nolisivat\nolit\nolin\nolimme\nolitte\nolivat\nollut\nolleet\nen\net\nei\nemme\n\
ette\neivät\nminä\nminun\nminut\nminua\nminussa\nminusta\nminuun\nminulla\nminulta\nminulle\n\
sinä\nsinun\nsinut\nsinua\nsinussa\nsinusta\nsinuun\nsinulla\nsinulta\nsinulle\nhän\nhänen\n\
hänet\nhäntä\nhänessä\nhänestä\nhäneen\nhänellä\nhäneltä\nhänelle\nme\nmeidän\nmeidät\n\
meitä\nmeissä\nmeistä\nmeihin\nmeillä\nmeiltä\nmeille\nte\nteidän\nteidät\nteitä\nteissä\n\
teistä\nteihin\nteillä\nteiltä\nteille\nhe\nheidän\nheidät\nheitä\nheissä\nheistä\nheihin\n\
heillä\nheiltä\nheille\ntämä\ntämän\ntätä\ntässä\ntästä\ntähän\ntällä\ntältä\ntälle\ntänä\n\
täksi\ntuo\ntuon\ntuota\ntuossa\ntuosta\ntuohon\ntuolla\ntuolta\ntuolle\ntuona\ntuoksi\nse\n\
sen\nsitä\nsiinä\nsiitä\nsiihen\nsillä\nsiltä\nsille\nsiksi\nnämä\nnäiden\nnäitä\nnäissä\n\
näistä\nnäihin\nnäillä\nnäiltä\nnäille\nnäinä\nnäiksi\nnuo\nnoiden\nnoita\nnoissa\nnoista\n\
noihin\nnoilla\nnoilta\nnoille\nnoina\nnoiksi\nne\nniiden\nniitä\nniissä\nniistä\nniihin\n\
niillä\nniiltä\nniille\nniinä\nniiksi\nkuka\nkenen\nkenet\nketä\nkenessä\nkenestä\nkeneen\n\
kenellä\nkeneltä\nkenelle\nkenenä\nkeneksi\nketkä\nkeiden\nkeitä\nkeissä\nkeistä\nkeihin\n\
keillä\nkeiltä\nkeille\nkeinä\nkeiksi\nmikä\nminkä\nmitä\nmissä\nmistä\nmihin\nmillä\nmiltä\n\
mille\nmiksi\nmitkä\njoka\njonka\njota\njossa\njosta\njohon\njolla\njolta\njolle\njona\n\
joksi\njotka\njoiden\njoita\njoissa\njoista\njoihin\njoilla\njoilta\njoille\njoina\njoiksi\n\
että\nja\njos\nkoska\nkuin\nmutta\nniin\nsekä\ntai\nvaan\nvai\nvaikka\nkanssa\nmukaan\nnoin\n\
poikki\nyli\nkun\nnyt\nitse\n";

/// `_french_` — 154 mots.
pub const FRENCH: &str = "\
au\naux\navec\nce\nces\ndans\nde\ndes\ndu\nelle\nen\net\neux\nil\nje\nla\nle\nleur\nlui\nma\n\
mais\nme\nmême\nmes\nmoi\nmon\nne\nnos\nnotre\nnous\non\nou\npar\npas\npour\nqu\nque\nqui\n\
sa\nse\nses\nsur\nta\nte\ntes\ntoi\nton\ntu\nun\nune\nvos\nvotre\nvous\nc\nd\nj\nl\nà\nm\nn\n\
s\nt\ny\nétée\nétées\nétant\nsuis\nes\nêtes\nsont\nserai\nseras\nsera\nserons\nserez\n\
seront\nserais\nserait\nserions\nseriez\nseraient\nétais\nétait\nétions\nétiez\nétaient\n\
fus\nfut\nfûmes\nfûtes\nfurent\nsois\nsoit\nsoyons\nsoyez\nsoient\nfusse\nfusses\nfussions\n\
fussiez\nfussent\nayant\neu\neue\neues\neus\nai\navons\navez\nont\naurai\naurons\naurez\n\
auront\naurais\naurait\naurions\nauriez\nauraient\navais\navait\naviez\navaient\neut\neûmes\n\
eûtes\neurent\naie\naies\nait\nayons\nayez\naient\neusse\neusses\neût\neussions\neussiez\n\
eussent\nceci\ncela\ncelà\ncet\ncette\nici\nils\nles\nleurs\nquel\nquels\nquelle\nquelles\n\
sans\nsoi\n";

/// `_german_` — 231 mots.
pub const GERMAN: &str = "\
aber\nalle\nallem\nallen\naller\nalles\nals\nalso\nam\nan\nander\nandere\nanderem\nanderen\n\
anderer\nanderes\nanderm\nandern\nanderr\nanders\nauch\nauf\naus\nbei\nbin\nbis\nbist\nda\n\
damit\ndann\nder\nden\ndes\ndem\ndie\ndas\ndaß\nderselbe\nderselben\ndenselben\ndesselben\n\
demselben\ndieselbe\ndieselben\ndasselbe\ndazu\ndein\ndeine\ndeinem\ndeinen\ndeiner\ndeines\n\
denn\nderer\ndessen\ndich\ndir\ndu\ndies\ndiese\ndiesem\ndiesen\ndieser\ndieses\ndoch\ndort\n\
durch\nein\neine\neinem\neinen\neiner\neines\neinig\neinige\neinigem\neinigen\neiniger\n\
einiges\neinmal\ner\nihn\nihm\nes\netwas\neuer\neure\neurem\neuren\neurer\neures\nfür\n\
gegen\ngewesen\nhab\nhabe\nhaben\nhat\nhatte\nhatten\nhier\nhin\nhinter\nich\nmich\nmir\n\
ihr\nihre\nihrem\nihren\nihrer\nihres\neuch\nim\nin\nindem\nins\nist\njede\njedem\njeden\n\
jeder\njedes\njene\njenem\njenen\njener\njenes\njetzt\nkann\nkein\nkeine\nkeinem\nkeinen\n\
keiner\nkeines\nkönnen\nkönnte\nmachen\nman\nmanche\nmanchem\nmanchen\nmancher\nmanches\n\
mein\nmeine\nmeinem\nmeinen\nmeiner\nmeines\nmit\nmuss\nmusste\nnach\nnicht\nnichts\nnoch\n\
nun\nnur\nob\noder\nohne\nsehr\nsein\nseine\nseinem\nseinen\nseiner\nseines\nselbst\nsich\n\
sie\nihnen\nsind\nso\nsolche\nsolchem\nsolchen\nsolcher\nsolches\nsoll\nsollte\nsondern\n\
sonst\nüber\num\nund\nuns\nunse\nunsem\nunsen\nunser\nunses\nunter\nviel\nvom\nvon\nvor\n\
während\nwar\nwaren\nwarst\nwas\nweg\nweil\nweiter\nwelche\nwelchem\nwelchen\nwelcher\n\
welches\nwenn\nwerde\nwerden\nwie\nwieder\nwill\nwir\nwird\nwirst\nwo\nwollen\nwollte\n\
würde\nwürden\nzu\nzum\nzur\nzwar\nzwischen\n";

/// `_hungarian_` — 198 mots.
pub const HUNGARIAN: &str = "\
a\nahogy\nahol\naki\nakik\nakkor\nalatt\náltal\náltalában\namely\namelyek\namelyekben\n\
amelyeket\namelyet\namelynek\nami\namit\namolyan\namíg\namikor\nát\nabban\nahhoz\nannak\n\
arra\narról\naz\nazok\nazon\nazt\nazzal\nazért\naztán\nazután\nazonban\nbár\nbe\nbelül\n\
benne\ncikk\ncikkek\ncikkeket\ncsak\nde\ne\neddig\negész\negy\negyes\negyetlen\negyéb\n\
egyik\negyre\nekkor\nel\nelég\nellen\nelő\nelőször\nelőtt\nelső\nén\néppen\nebben\nehhez\n\
emilyen\nennek\nerre\nez\nezt\nezek\nezen\nezzel\nezért\nés\nfel\nfelé\nhanem\nhiszen\nhogy\n\
hogyan\nigen\nígy\nilletve\nill.\nill\nilyen\nilyenkor\nison\nismét\nitt\njó\njól\njobban\n\
kell\nkellett\nkeresztül\nkeressünk\nki\nkívül\nközött\nközül\nlegalább\nlehet\nlehetett\n\
legyen\nlenne\nlenni\nlesz\nlett\nmaga\nmagát\nmajd\nmár\nmás\nmásik\nmeg\nmég\nmellett\n\
mert\nmely\nmelyek\nmi\nmit\nmíg\nmiért\nmilyen\nmikor\nminden\nmindent\nmindenki\nmindig\n\
mint\nmintha\nmivel\nmost\nnagy\nnagyobb\nnagyon\nne\nnéha\nnekem\nneki\nnem\nnéhány\n\
nélkül\nnincs\nolyan\nott\nössze\nő\nők\nőket\npedig\npersze\nrá\ns\nsaját\nsem\nsemmi\nsok\n\
sokat\nsokkal\nszámára\nszemben\nszerint\nszinte\ntalán\ntehát\nteljes\ntovább\ntovábbá\n\
több\núgy\nugyanis\núj\nújabb\nújra\nután\nutána\nutolsó\nvagy\nvagyis\nvalaki\nvalami\n\
valamint\nvaló\nvagyok\nvan\nvannak\nvolt\nvoltam\nvoltak\nvoltunk\nvissza\nvele\nviszont\n\
volna\n";

/// `_italian_` — 279 mots.
pub const ITALIAN: &str = "\
ad\nal\nallo\nai\nagli\nall\nagl\nalla\nalle\ncon\ncol\ncoi\nda\ndal\ndallo\ndai\ndagli\n\
dall\ndagl\ndalla\ndalle\ndi\ndel\ndello\ndei\ndegli\ndell\ndegl\ndella\ndelle\nin\nnel\n\
nello\nnei\nnegli\nnell\nnegl\nnella\nnelle\nsu\nsul\nsullo\nsui\nsugli\nsull\nsugl\nsulla\n\
sulle\nper\ntra\ncontro\nio\ntu\nlui\nlei\nnoi\nvoi\nloro\nmio\nmia\nmiei\nmie\ntuo\ntua\n\
tuoi\ntue\nsuo\nsua\nsuoi\nsue\nnostro\nnostra\nnostri\nnostre\nvostro\nvostra\nvostri\n\
vostre\nmi\nti\nci\nvi\nlo\nla\nli\nle\ngli\nne\nil\nun\nuno\nuna\nma\ned\nse\nperché\n\
anche\ncome\ndov\ndove\nche\nchi\ncui\nnon\npiù\nquale\nquanto\nquanti\nquanta\nquante\n\
quello\nquelli\nquella\nquelle\nquesto\nquesti\nquesta\nqueste\nsi\ntutto\ntutti\na\nc\ne\n\
i\nl\no\nho\nhai\nha\nabbiamo\navete\nhanno\nabbia\nabbiate\nabbiano\navrò\navrai\navrà\n\
avremo\navrete\navranno\navrei\navresti\navrebbe\navremmo\navreste\navrebbero\navevo\navevi\n\
aveva\navevamo\navevate\navevano\nebbi\navesti\nebbe\navemmo\naveste\nebbero\navessi\n\
avesse\navessimo\navessero\navendo\navuto\navuta\navuti\navute\nsono\nsei\nè\nsiamo\nsiete\n\
sia\nsiate\nsiano\nsarò\nsarai\nsarà\nsaremo\nsarete\nsaranno\nsarei\nsaresti\nsarebbe\n\
saremmo\nsareste\nsarebbero\nero\neri\nera\neravamo\neravate\nerano\nfui\nfosti\nfu\nfummo\n\
foste\nfurono\nfossi\nfosse\nfossimo\nfossero\nessendo\nfaccio\nfai\nfacciamo\nfanno\n\
faccia\nfacciate\nfacciano\nfarò\nfarai\nfarà\nfaremo\nfarete\nfaranno\nfarei\nfaresti\n\
farebbe\nfaremmo\nfareste\nfarebbero\nfacevo\nfacevi\nfaceva\nfacevamo\nfacevate\nfacevano\n\
feci\nfacesti\nfece\nfacemmo\nfaceste\nfecero\nfacessi\nfacesse\nfacessimo\nfacessero\n\
facendo\nsto\nstai\nsta\nstiamo\nstanno\nstia\nstiate\nstiano\nstarò\nstarai\nstarà\n\
staremo\nstarete\nstaranno\nstarei\nstaresti\nstarebbe\nstaremmo\nstareste\nstarebbero\n\
stavo\nstavi\nstava\nstavamo\nstavate\nstavano\nstetti\nstesti\nstette\nstemmo\nsteste\n\
stettero\nstessi\nstesse\nstessimo\nstessero\nstando\n";

/// `_norwegian_` — 172 mots.
pub const NORWEGIAN: &str = "\
og\ni\njeg\ndet\nat\nen\net\nden\ntil\ner\nsom\npå\nde\nmed\nhan\nav\nikke\nikkje\nder\nså\n\
var\nmeg\nseg\nmen\nett\nhar\nom\nvi\nmin\nmitt\nha\nhadde\nhun\nnå\nover\nda\nved\nfra\ndu\n\
ut\nsin\ndem\noss\nopp\nman\nkan\nhans\nhvor\neller\nhva\nskal\nselv\nsjøl\nher\nalle\nvil\n\
bli\nble\nblei\nblitt\nkunne\ninn\nnår\nvære\nkom\nnoen\nnoe\nville\ndere\nderes\nkun\nja\n\
etter\nned\nskulle\ndenne\nfor\ndeg\nsi\nsine\nsitt\nmot\nå\nmeget\nhvorfor\ndette\ndisse\n\
uten\nhvordan\ningen\ndin\nditt\nblir\nsamme\nhvilken\nhvilke\nsånn\ninni\nmellom\nvår\n\
hver\nhvem\nvors\nhvis\nbåde\nbare\nenn\nfordi\nfør\nmange\nogså\nslik\nvært\nbåe\nbegge\n\
siden\ndykk\ndykkar\ndei\ndeira\ndeires\ndeim\ndi\ndå\neg\nein\neit\neitt\nelles\nhonom\n\
hjå\nho\nhoe\nhenne\nhennar\nhennes\nhoss\nhossen\ningi\ninkje\nkorleis\nkorso\nkva\nkvar\n\
kvarhelst\nkven\nkvi\nkvifor\nme\nmedan\nmi\nmine\nmykje\nno\nnokon\nnoka\nnokor\nnoko\n\
nokre\nsia\nsidan\nso\nsomt\nsomme\num\nupp\nvere\nvore\nverte\nvort\nvarte\nvart\n";

/// `_portuguese_` — 203 mots.
pub const PORTUGUESE: &str = "\
de\na\no\nque\ne\ndo\nda\nem\num\npara\ncom\nnão\numa\nos\nno\nse\nna\npor\nmais\nas\ndos\n\
como\nmas\nao\nele\ndas\nà\nseu\nsua\nou\nquando\nmuito\nnos\njá\neu\ntambém\nsó\npelo\n\
pela\naté\nisso\nela\nentre\ndepois\nsem\nmesmo\naos\nseus\nquem\nnas\nme\nesse\neles\nvocê\n\
essa\nnum\nnem\nsuas\nmeu\nàs\nminha\nnuma\npelos\nelas\nqual\nnós\nlhe\ndeles\nessas\n\
esses\npelas\neste\ndele\ntu\nte\nvocês\nvos\nlhes\nmeus\nminhas\nteu\ntua\nteus\ntuas\n\
nosso\nnossa\nnossos\nnossas\ndela\ndelas\nesta\nestes\nestas\naquele\naquela\naqueles\n\
aquelas\nisto\naquilo\nestou\nestá\nestamos\nestão\nestive\nesteve\nestivemos\nestiveram\n\
estava\nestávamos\nestavam\nestivera\nestivéramos\nesteja\nestejamos\nestejam\nestivesse\n\
estivéssemos\nestivessem\nestiver\nestivermos\nestiverem\nhei\nhá\nhavemos\nhão\nhouve\n\
houvemos\nhouveram\nhouvera\nhouvéramos\nhaja\nhajamos\nhajam\nhouvesse\nhouvéssemos\n\
houvessem\nhouver\nhouvermos\nhouverem\nhouverei\nhouverá\nhouveremos\nhouverão\nhouveria\n\
houveríamos\nhouveriam\nsou\nsomos\nsão\nera\néramos\neram\nfui\nfoi\nfomos\nforam\nfora\n\
fôramos\nseja\nsejamos\nsejam\nfosse\nfôssemos\nfossem\nfor\nformos\nforem\nserei\nserá\n\
seremos\nserão\nseria\nseríamos\nseriam\ntenho\ntem\ntemos\ntém\ntinha\ntínhamos\ntinham\n\
tive\nteve\ntivemos\ntiveram\ntivera\ntivéramos\ntenha\ntenhamos\ntenham\ntivesse\n\
tivéssemos\ntivessem\ntiver\ntivermos\ntiverem\nterei\nterá\nteremos\nterão\nteria\n\
teríamos\nteriam\n";

/// `_romanian_` — 230 mots.
pub const ROMANIAN: &str = "\
acea\naceasta\naceastă\naceea\nacei\naceia\nacel\nacela\nacele\nacelea\nacest\nacesta\n\
aceste\nacestea\naceşti\naceştia\nacolo\nacum\nai\naia\naibă\naici\nal\năla\nale\nalea\n\
ălea\naltceva\naltcineva\nam\nar\nare\naş\naşadar\nasemenea\nasta\năsta\nastăzi\nastea\n\
ăstea\năştia\nasupra\naţi\nau\navea\navem\naveţi\nazi\nbine\nbucur\nbună\nca\ncă\ncăci\n\
când\ncare\ncărei\ncăror\ncărui\ncât\ncâte\ncâţi\ncătre\ncâtva\nce\ncel\nceva\nchiar\ncînd\n\
cine\ncineva\ncît\ncîte\ncîţi\ncîtva\ncontra\ncu\ncum\ncumva\ncurând\ncurînd\nda\ndă\ndacă\n\
dar\ndatorită\nde\ndeci\ndeja\ndeoarece\ndeparte\ndeşi\ndin\ndinaintea\ndintr\ndintre\n\
drept\ndupă\nea\nei\nel\nele\neram\neste\neşti\neu\nface\nfără\nfi\nfie\nfiecare\nfii\nfim\n\
fiţi\niar\nieri\nîi\nîl\nîmi\nîmpotriva\nîn\nînainte\nînaintea\nîncât\nîncît\nîncotro\n\
între\nîntrucât\nîntrucît\nîţi\nla\nlângă\nle\nli\nlîngă\nlor\nlui\nmă\nmâine\nmea\nmei\n\
mele\nmereu\nmeu\nmi\nmine\nmult\nmultă\nmulţi\nne\nnicăieri\nnici\nnimeni\nnişte\nnoastră\n\
noastre\nnoi\nnoştri\nnostru\nnu\nori\noricând\noricare\noricât\norice\noricînd\noricine\n\
oricît\noricum\noriunde\npână\npe\npentru\npeste\npînă\npoate\npot\nprea\nprima\nprimul\n\
prin\nprintr\nsa\nsă\nsăi\nsale\nsau\nsău\nse\nşi\nsînt\nsîntem\nsînteţi\nspre\nsub\nsunt\n\
suntem\nsunteţi\nta\ntăi\ntale\ntău\nte\nţi\nţie\ntine\ntoată\ntoate\ntot\ntoţi\ntotuşi\ntu\n\
un\nuna\nunde\nundeva\nunei\nunele\nuneori\nunor\nvă\nvi\nvoastră\nvoastre\nvoi\nvoştri\n\
vostru\nvouă\nvreo\nvreun\n";

/// `_russian_` — 159 mots.
pub const RUSSIAN: &str = "\
и\nв\nво\nне\nчто\nон\nна\nя\nс\nсо\nкак\nа\nто\nвсе\nона\nтак\nего\nно\nда\nты\nк\nу\nже\n\
вы\nза\nбы\nпо\nтолько\nее\nмне\nбыло\nвот\nот\nменя\nеще\nнет\nо\nиз\nему\nтеперь\nкогда\n\
даже\nну\nвдруг\nли\nесли\nуже\nили\nни\nбыть\nбыл\nнего\nдо\nвас\nнибудь\nопять\nуж\nвам\n\
сказал\nведь\nтам\nпотом\nсебя\nничего\nей\nможет\nони\nтут\nгде\nесть\nнадо\nней\nдля\nмы\n\
тебя\nих\nчем\nбыла\nсам\nчтоб\nбез\nбудто\nчеловек\nчего\nраз\nтоже\nсебе\nпод\nжизнь\n\
будет\nж\nтогда\nкто\nэтот\nговорил\nтого\nпотому\nэтого\nкакой\nсовсем\nним\nздесь\nэтом\n\
один\nпочти\nмой\nтем\nчтобы\nнее\nкажется\nсейчас\nбыли\nкуда\nзачем\nсказать\nвсех\n\
никогда\nсегодня\nможно\nпри\nнаконец\nдва\nоб\nдругой\nхоть\nпосле\nнад\nбольше\nтот\n\
через\nэти\nнас\nпро\nвсего\nних\nкакая\nмного\nразве\nсказала\nтри\nэту\nмоя\nвпрочем\n\
хорошо\nсвою\nэтой\nперед\nиногда\nлучше\nчуть\nтом\nнельзя\nтакой\nим\nболее\nвсегда\n\
конечно\nвсю\nмежду\n";

/// `_spanish_` — 308 mots.
pub const SPANISH: &str = "\
de\nla\nque\nel\nen\ny\na\nlos\ndel\nse\nlas\npor\nun\npara\ncon\nno\nuna\nsu\nal\nlo\ncomo\n\
más\npero\nsus\nle\nya\no\neste\nsí\nporque\nesta\nentre\ncuando\nmuy\nsin\nsobre\ntambién\n\
me\nhasta\nhay\ndonde\nquien\ndesde\ntodo\nnos\ndurante\ntodos\nuno\nles\nni\ncontra\notros\n\
ese\neso\nante\nellos\ne\nesto\nmí\nantes\nalgunos\nqué\nunos\nyo\notro\notras\notra\nél\n\
tanto\nesa\nestos\nmucho\nquienes\nnada\nmuchos\ncual\npoco\nella\nestar\nestas\nalgunas\n\
algo\nnosotros\nmi\nmis\ntú\nte\nti\ntu\ntus\nellas\nnosotras\nvosotros\nvosotras\nos\nmío\n\
mía\nmíos\nmías\ntuyo\ntuya\ntuyos\ntuyas\nsuyo\nsuya\nsuyos\nsuyas\nnuestro\nnuestra\n\
nuestros\nnuestras\nvuestro\nvuestra\nvuestros\nvuestras\nesos\nesas\nestoy\nestás\nestá\n\
estamos\nestáis\nestán\nesté\nestés\nestemos\nestéis\nestén\nestaré\nestarás\nestará\n\
estaremos\nestaréis\nestarán\nestaría\nestarías\nestaríamos\nestaríais\nestarían\nestaba\n\
estabas\nestábamos\nestabais\nestaban\nestuve\nestuviste\nestuvo\nestuvimos\nestuvisteis\n\
estuvieron\nestuviera\nestuvieras\nestuviéramos\nestuvierais\nestuvieran\nestuviese\n\
estuvieses\nestuviésemos\nestuvieseis\nestuviesen\nestando\nestado\nestada\nestados\n\
estadas\nestad\nhe\nhas\nha\nhemos\nhabéis\nhan\nhaya\nhayas\nhayamos\nhayáis\nhayan\nhabré\n\
habrás\nhabrá\nhabremos\nhabréis\nhabrán\nhabría\nhabrías\nhabríamos\nhabríais\nhabrían\n\
había\nhabías\nhabíamos\nhabíais\nhabían\nhube\nhubiste\nhubo\nhubimos\nhubisteis\nhubieron\n\
hubiera\nhubieras\nhubiéramos\nhubierais\nhubieran\nhubiese\nhubieses\nhubiésemos\n\
hubieseis\nhubiesen\nhabiendo\nhabido\nhabida\nhabidos\nhabidas\nsoy\neres\nes\nsomos\nsois\n\
son\nsea\nseas\nseamos\nseáis\nsean\nseré\nserás\nserá\nseremos\nseréis\nserán\nsería\n\
serías\nseríamos\nseríais\nserían\nera\neras\néramos\nerais\neran\nfui\nfuiste\nfue\nfuimos\n\
fuisteis\nfueron\nfuera\nfueras\nfuéramos\nfuerais\nfueran\nfuese\nfueses\nfuésemos\n\
fueseis\nfuesen\nsiendo\nsido\ntengo\ntienes\ntiene\ntenemos\ntenéis\ntienen\ntenga\ntengas\n\
tengamos\ntengáis\ntengan\ntendré\ntendrás\ntendrá\ntendremos\ntendréis\ntendrán\ntendría\n\
tendrías\ntendríamos\ntendríais\ntendrían\ntenía\ntenías\nteníamos\nteníais\ntenían\ntuve\n\
tuviste\ntuvo\ntuvimos\ntuvisteis\ntuvieron\ntuviera\ntuvieras\ntuviéramos\ntuvierais\n\
tuvieran\ntuviese\ntuvieses\ntuviésemos\ntuvieseis\ntuviesen\nteniendo\ntenido\ntenida\n\
tenidos\ntenidas\ntened\n";

/// `_swedish_` — 114 mots.
pub const SWEDISH: &str = "\
och\ndet\natt\ni\nen\njag\nhon\nsom\nhan\npå\nden\nmed\nvar\nsig\nför\nså\ntill\när\nmen\n\
ett\nom\nhade\nde\nav\nicke\nmig\ndu\nhenne\ndå\nsin\nnu\nhar\ninte\nhans\nhonom\nskulle\n\
hennes\ndär\nmin\nman\nej\nvid\nkunde\nnågot\nfrån\nut\nnär\nefter\nupp\nvi\ndem\nvara\nvad\n\
över\nän\ndig\nkan\nsina\nhär\nha\nmot\nalla\nunder\nnågon\neller\nallt\nmycket\nsedan\nju\n\
denna\nsjälv\ndetta\nåt\nutan\nvarit\nhur\ningen\nmitt\nni\nbli\nblev\noss\ndin\ndessa\n\
några\nderas\nblir\nmina\nsamma\nvilken\ner\nsådan\nvår\nblivit\ndess\ninom\nmellan\nsådant\n\
varför\nvarje\nvilka\nditt\nvem\nvilket\nsitta\nsådana\nvart\ndina\nvars\nvårt\nvåra\nert\n\
era\nvilkas\n";

/// `_turkish_` — 209 mots.
pub const TURKISH: &str = "\
acaba\naltmış\naltı\nama\nancak\narada\naslında\nayrıca\nbana\nbazı\nbelki\nben\nbenden\n\
beni\nbenim\nberi\nbeş\nbile\nbin\nbir\nbirçok\nbiri\nbirkaç\nbirkez\nbirşey\nbirşeyi\nbiz\n\
bize\nbizden\nbizi\nbizim\nböyle\nböylece\nbu\nbuna\nbunda\nbundan\nbunlar\nbunları\n\
bunların\nbunu\nbunun\nburada\nçok\nçünkü\nda\ndaha\ndahi\nde\ndefa\ndeğil\ndiğer\ndiye\n\
doksan\ndokuz\ndolayı\ndolayısıyla\ndört\nedecek\neden\nederek\nedilecek\nediliyor\n\
edilmesi\nediyor\neğer\nelli\nen\netmesi\netti\nettiği\nettiğini\ngibi\ngöre\nhalen\nhangi\n\
hatta\nhem\nhenüz\nhep\nhepsi\nher\nherhangi\nherkesin\nhiç\nhiçbir\niçin\niki\nile\nilgili\n\
ise\nişte\nitibaren\nitibariyle\nkadar\nkarşın\nkatrilyon\nkendi\nkendilerine\nkendini\n\
kendisi\nkendisine\nkendisini\nkez\nki\nkim\nkimden\nkime\nkimi\nkimse\nkırk\nmilyar\n\
milyon\nmu\nmü\nmı\nnasıl\nne\nneden\nnedenle\nnerde\nnerede\nnereye\nniye\nniçin\no\nolan\n\
olarak\noldu\nolduğu\nolduğunu\nolduklarını\nolmadı\nolmadığı\nolmak\nolması\nolmayan\n\
olmaz\nolsa\nolsun\nolup\nolur\nolursa\noluyor\non\nona\nondan\nonlar\nonlardan\nonları\n\
onların\nonu\nonun\notuz\noysa\nöyle\npek\nrağmen\nsadece\nsanki\nsekiz\nseksen\nsen\n\
senden\nseni\nsenin\nsiz\nsizden\nsizi\nsizin\nşey\nşeyden\nşeyi\nşeyler\nşöyle\nşu\nşuna\n\
şunda\nşundan\nşunları\nşunu\ntarafından\ntrilyon\ntüm\nüç\nüzere\nvar\nvardı\nve\nveya\nya\n\
yani\nyapacak\nyapılan\nyapılması\nyapıyor\nyapmak\nyaptı\nyaptığı\nyaptığını\nyaptıkları\n\
yedi\nyerine\nyetmiş\nyine\nyirmi\nyoksa\nyüz\nzaten\n";
