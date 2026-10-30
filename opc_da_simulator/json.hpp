#pragma once
// Original small JSON implementation: RFC 8259 grammar, UTF-8, surrogate pairs,
// duplicate-key rejection, bounded nesting, and finite numeric values.
#include <cmath>
#include <cstdlib>
#include <iomanip>
#include <limits>
#include <locale>
#include <map>
#include <sstream>
#include <stdexcept>
#include <string>
#include <vector>

namespace json {
struct Value {
    enum Type { Null, Boolean, Number, String, Array, Object } type = Null;
    bool boolean = false;
    double number = 0;
    std::string string;
    std::vector<Value> array;
    std::map<std::string,Value> object;
    Value() = default;
    explicit Value(bool v): type(Boolean), boolean(v) {}
    explicit Value(double v): type(Number), number(v) {
        if (!std::isfinite(v)) throw std::runtime_error("non-finite JSON number");
    }
    explicit Value(std::string v): type(String), string(std::move(v)) {}
    explicit Value(const char* v): Value(std::string(v)) {}
    static Value list() { Value v; v.type=Array; return v; }
    static Value dict() { Value v; v.type=Object; return v; }
    const Value& at(const std::string& key) const {
        if(type!=Object) throw std::runtime_error("expected JSON object");
        auto it=object.find(key);
        if(it==object.end()) throw std::runtime_error("missing field: "+key);
        return it->second;
    }
    const Value* find(const std::string& key) const {
        if(type!=Object) throw std::runtime_error("expected JSON object");
        auto it=object.find(key); return it==object.end()?nullptr:&it->second;
    }
    const std::string& text() const {
        if(type!=String) throw std::runtime_error("expected JSON string"); return string;
    }
    double numeric() const {
        if(type!=Number) throw std::runtime_error("expected JSON number"); return number;
    }
    bool truth() const {
        if(type!=Boolean) throw std::runtime_error("expected JSON boolean"); return boolean;
    }
    const std::vector<Value>& elements() const {
        if(type!=Array) throw std::runtime_error("expected JSON array"); return array;
    }
};
inline void append_utf8(std::string& out, unsigned code) {
    if(code<=0x7f) out+=static_cast<char>(code);
    else if(code<=0x7ff) { out+=static_cast<char>(0xc0|(code>>6)); out+=static_cast<char>(0x80|(code&63)); }
    else if(code<=0xffff) { out+=static_cast<char>(0xe0|(code>>12)); out+=static_cast<char>(0x80|((code>>6)&63)); out+=static_cast<char>(0x80|(code&63)); }
    else { out+=static_cast<char>(0xf0|(code>>18)); out+=static_cast<char>(0x80|((code>>12)&63)); out+=static_cast<char>(0x80|((code>>6)&63)); out+=static_cast<char>(0x80|(code&63)); }
}
class Parser {
    const std::string& source;
    size_t pos=0;
    [[noreturn]] void fail(const char* message) const {
        throw std::runtime_error(std::string(message)+" at byte "+std::to_string(pos));
    }
    void space() { while(pos<source.size() && (source[pos]==' '||source[pos]=='\t'||source[pos]=='\r'||source[pos]=='\n')) ++pos; }
    bool take(char c) { if(pos<source.size() && source[pos]==c) { ++pos; return true; } return false; }
    void require(char c) { if(!take(c)) fail("unexpected token"); }
    unsigned hex4() {
        unsigned n=0;
        for(int i=0;i<4;++i) {
            if(pos==source.size()) fail("truncated Unicode escape");
            const char c=source[pos++]; unsigned d;
            if(c>='0'&&c<='9') d=c-'0'; else if(c>='a'&&c<='f') d=c-'a'+10;
            else if(c>='A'&&c<='F') d=c-'A'+10; else fail("invalid Unicode escape");
            n=(n<<4)|d;
        }
        return n;
    }
    std::string string() {
        require('"'); std::string out;
        while(pos<source.size()) {
            const unsigned char c=source[pos++];
            if(c=='"') return out;
            if(c<0x20) fail("control character in string");
            if(c=='\\') {
                if(pos==source.size()) fail("truncated escape");
                switch(source[pos++]) {
                    case '"': out+='"'; break; case '\\': out+='\\'; break; case '/': out+='/'; break;
                    case 'b': out+='\b'; break; case 'f': out+='\f'; break; case 'n': out+='\n'; break;
                    case 'r': out+='\r'; break; case 't': out+='\t'; break;
                    case 'u': {
                        unsigned code=hex4();
                        if(code>=0xd800&&code<=0xdbff) {
                            require('\\'); require('u'); const unsigned low=hex4();
                            if(low<0xdc00||low>0xdfff) fail("invalid surrogate pair");
                            code=0x10000+((code-0xd800)<<10)+(low-0xdc00);
                        } else if(code>=0xdc00&&code<=0xdfff) fail("unpaired surrogate");
                        append_utf8(out,code); break;
                    }
                    default: fail("invalid escape");
                }
            } else if(c<0x80) out+=static_cast<char>(c);
            else {
                unsigned code, count, minimum;
                if(c>=0xc2&&c<=0xdf) { code=c&31; count=1; minimum=0x80; }
                else if(c>=0xe0&&c<=0xef) { code=c&15; count=2; minimum=0x800; }
                else if(c>=0xf0&&c<=0xf4) { code=c&7; count=3; minimum=0x10000; }
                else fail("invalid UTF-8 leading byte");
                for(unsigned j=0;j<count;++j) {
                    if(pos==source.size()) fail("truncated UTF-8");
                    unsigned char d=source[pos++]; if((d&0xc0)!=0x80) fail("invalid UTF-8 continuation");
                    code=(code<<6)|(d&63);
                }
                if(code<minimum||code>0x10ffff||(code>=0xd800&&code<=0xdfff)) fail("invalid UTF-8 scalar");
                append_utf8(out,code);
            }
        }
        fail("unterminated string");
    }
    Value value(unsigned depth) {
        if(depth>32) fail("JSON nesting limit exceeded");
        space(); if(pos==source.size()) fail("missing value");
        if(source[pos]=='"') return Value(string());
        if(take('[')) {
            Value out=Value::list(); space(); if(take(']')) return out;
            do { out.array.push_back(value(depth+1)); space(); if(take(']')) return out; require(','); } while(true);
        }
        if(take('{')) {
            Value out=Value::dict(); space(); if(take('}')) return out;
            do {
                space(); auto key=string(); space(); require(':'); auto v=value(depth+1);
                if(!out.object.emplace(std::move(key),std::move(v)).second) fail("duplicate object key");
                space(); if(take('}')) return out; require(',');
            } while(true);
        }
        for(const auto& literal : {std::string("true"),std::string("false"),std::string("null")}) {
            if(source.compare(pos,literal.size(),literal)==0) {
                pos+=literal.size(); return literal=="null"?Value():Value(literal=="true");
            }
        }
        const size_t start=pos; take('-');
        if(take('0')) { if(pos<source.size()&&source[pos]>='0'&&source[pos]<='9') fail("leading zero"); }
        else {
            if(pos==source.size()||source[pos]<'1'||source[pos]>'9') fail("invalid number");
            while(pos<source.size()&&source[pos]>='0'&&source[pos]<='9') ++pos;
        }
        if(take('.')) {
            size_t begin=pos; while(pos<source.size()&&source[pos]>='0'&&source[pos]<='9') ++pos;
            if(begin==pos) fail("missing fraction digits");
        }
        if(take('e')||take('E')) {
            if(!take('+')) take('-');
            size_t begin=pos; while(pos<source.size()&&source[pos]>='0'&&source[pos]<='9') ++pos;
            if(begin==pos) fail("missing exponent digits");
        }
        const std::string token=source.substr(start,pos-start);
        std::istringstream stream(token); stream.imbue(std::locale::classic()); double n;
        stream>>n; if(stream.fail()||!std::isfinite(n)) fail("number outside finite double range");
        return Value(n);
    }
public:
    explicit Parser(const std::string& input):source(input) {}
    Value parse() { Value v=value(0); space(); if(pos!=source.size()) fail("trailing content"); return v; }
};
inline void quote(std::ostream& out,const std::string& s) {
    out<<'"'; const char* hex="0123456789abcdef";
    for(unsigned char c:s) {
        if(c=='"'||c=='\\') out<<'\\'<<static_cast<char>(c);
        else if(c<0x20) out<<"\\u00"<<hex[c>>4]<<hex[c&15]; else out<<static_cast<char>(c);
    }
    out<<'"';
}
inline void serialize(std::ostream& out,const Value& v) {
    switch(v.type) {
        case Value::Null: out<<"null"; break;
        case Value::Boolean: out<<(v.boolean?"true":"false"); break;
        case Value::Number: out<<std::setprecision(std::numeric_limits<double>::max_digits10)<<v.number; break;
        case Value::String: quote(out,v.string); break;
        case Value::Array: {
            out<<'['; bool first=true;
            for(const auto& x:v.array) { if(!first) out<<','; first=false; serialize(out,x); } out<<']'; break;
        }
        case Value::Object: {
            out<<'{'; bool first=true;
            for(const auto& x:v.object) { if(!first) out<<','; first=false; quote(out,x.first); out<<':'; serialize(out,x.second); } out<<'}'; break;
        }
    }
}
inline std::string dump(const Value& v) {
    std::ostringstream out; out.imbue(std::locale::classic()); serialize(out,v); return out.str();
}
}
